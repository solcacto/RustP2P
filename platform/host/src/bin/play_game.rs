use anyhow::{bail, Context, Result};
use host::{
    avatar_state::{AvatarPose, AvatarState},
    avatar_standard,
    host_functions,
    host_state::HostState,
    manifest::GameManifest,
    peer_connection::PeerConnection,
    renderer,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wasmtime::{Engine, Linker, Module, Store};

const SIGNAL_SERVER: &str = "127.0.0.1:9001";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const GAME_PACKAGE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../guest");
const MANIFEST_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../guest/game_manifest.json");
const AVATAR_ASSET_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");

/// Commit 12: loads a game *package* — a `game_manifest.json` plus its Wasm —
/// validates the manifest, refuses to load if the pinned SHA-256 doesn't match,
/// then runs the Chase/Tag session with real keyboard input and a score HUD.
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let role = args
        .iter()
        .position(|a| a == "--role")
        .map(|i| args[i + 1].clone())
        .unwrap_or_else(|| "A".to_string());
    if role != "A" && role != "B" {
        bail!("usage: play_game --role A|B [--avatar <path>] [--cosmetic <Point>:<kind>] [--auto] [--frames N] [--no-exit]");
    }
    let local_id = format!("Peer{role}");
    let remote_id = if role == "A" { "PeerB" } else { "PeerA" };
    let axis: u8 = if role == "A" { 0 } else { 1 };
    let auto = args.iter().any(|a| a == "--auto");
    let no_exit = args.iter().any(|a| a == "--no-exit");
    let frames = args
        .iter()
        .position(|a| a == "--frames")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(900);
    // Avatar asset path, relative to the host asset folder.
    let avatar_path = args
        .iter()
        .position(|a| a == "--avatar")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "avatar_standard.glb".to_string());
    // Cosmetic slots, repeatable: --cosmetic <Point>:<kind>, e.g. Head:hat.
    let cosmetics: Vec<(renderer::AttachmentPoint, renderer::CosmeticKind)> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == "--cosmetic")
        .filter_map(|(i, _)| args.get(i + 1))
        .filter_map(|spec| {
            let (point, kind) = spec.split_once(':')?;
            Some((renderer::AttachmentPoint::parse(point)?, renderer::CosmeticKind::parse(kind)?))
        })
        .collect();

    // Shared pose bridges between the Wasm guest, the network poll, and Bevy.
    let avatar_state = Arc::new(Mutex::new(AvatarState::default()));
    let remote_avatars: Arc<Mutex<HashMap<String, AvatarPose>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Load and validate the game package manifest, then load the Wasm it pins
    // and verify the artifact's hash before trusting a single byte.
    let manifest = GameManifest::from_path(MANIFEST_PATH)?;
    println!(
        "[{role}] game: {} v{} by {} (mode={:?}, max_players={})",
        manifest.name, manifest.version, manifest.author, manifest.mode, manifest.max_players
    );
    let wasm_path = Path::new(GAME_PACKAGE_DIR).join(&manifest.wasm_entry);
    let wasm_bytes = std::fs::read(&wasm_path).with_context(|| {
        format!(
            "{} not found in game package {} — build the guest first:\n  \
             cargo build -p guest --target wasm32-unknown-unknown --release && \
             cp target/wasm32-unknown-unknown/release/guest.wasm platform/guest/guest.wasm",
            manifest.wasm_entry,
            GAME_PACKAGE_DIR
        )
    })?;
    manifest.verify_wasm(&manifest.wasm_entry, &wasm_bytes)?;
    let wasm_size = wasm_bytes.len();

    // The avatar is loaded from a configurable path before the game starts and
    // validated against the avatar standard; a non-conforming avatar refuses to
    // load.
    let avatar_file = std::path::Path::new(AVATAR_ASSET_DIR).join(&avatar_path);
    let avatar_report = avatar_standard::validate_avatar_path(&avatar_file)?;
    println!(
        "[{role}] avatar '{avatar_path}': {} bones, {} triangles, {} textures, {} animations",
        avatar_report.bone_count,
        avatar_report.triangle_count,
        avatar_report.texture_count,
        avatar_report.animations.len()
    );
    let cosmetic_desc: Vec<String> = cosmetics
        .iter()
        .map(|(p, k)| format!("{}:{:?}", p.as_str(), k))
        .collect();
    println!("[{role}] cosmetics: {}", if cosmetic_desc.is_empty() { "none".to_string() } else { cosmetic_desc.join(", ") });

    let engine = Engine::default();
    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let module = Module::new(&engine, wasm_bytes)?;
    let mut store = Store::new(&engine, HostState::new(local_id.clone()));
    store.data_mut().set_avatar_state(Some(avatar_state.clone()));
    store.data_mut().set_remote_avatars(remote_avatars.clone());
    store.data_mut().set_movement_axis(axis);
    store.data_mut().set_avatar_path(avatar_path.clone());
    store.data_mut().set_peer_connection(Some(connect_with_retry(&local_id)?));
    let instance = linker.instantiate(&mut store, &module)?;
    let game_tick = instance.get_typed_func::<(), ()>(&mut store, "game_tick")?;
    println!(
        "[{role}] verified + loaded {} ({wasm_size} bytes, hash OK)",
        manifest.wasm_entry
    );

    // Block until the other instance is registered and a P2P link is up.
    println!("[{role}] connecting to {remote_id} via signaling server...");
    connect_to_peer(&mut store, remote_id)?;
    println!("[{role}] connected to {remote_id}");

    // Build the Bevy renderer: one guest `game_tick` per rendered frame.
    let mut app = renderer::build_app(avatar_state.clone(), remote_avatars.clone());
    let store_handle = Arc::new(Mutex::new(store));
    app.insert_resource(renderer::AutoInput(auto));
    app.insert_resource(renderer::CosmeticSlots(cosmetics));
    app.insert_resource(renderer::WasmRuntime {
        store: store_handle.clone(),
        render_tick: game_tick,
    });
    if !no_exit {
        renderer::add_exit_after(&mut app, frames);
    }

    let exit = app.run();
    if !exit.is_success() {
        bail!("renderer exited with an error: {exit:?}");
    }

    // Graceful shutdown: close the signaling WebSocket and UDP socket before
    // the process exits so the server observes a clean disconnect.
    let mut guard = store_handle.lock().unwrap();
    if let Some(pc) = guard.data_mut().peer_connection_mut() {
        match pc.shutdown() {
            Ok(_) => println!("[{role}] signaling connection closed cleanly"),
            Err(e) => println!("[{role}] warning: signaling shutdown: {e}"),
        }
    }
    drop(guard);

    // Post-run verification.
    let rendered = store_handle.lock().unwrap().data().frame_count();
    println!("[{role}] rendered {rendered} frames");
    let mut guard = store_handle.lock().unwrap();
    let guest_score =
        instance.get_typed_func::<(), i32>(&mut *guard, "get_tag_score")?.call(&mut *guard, ())?;
    let seen = instance
        .get_typed_func::<(), i32>(&mut *guard, "remote_pose_seen")?
        .call(&mut *guard, ())?;
    let local = guard.data().tag_score();
    let remote_scores: Vec<String> = guard
        .data()
        .remote_scores()
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    println!(
        "[{role}] guest score: {guest_score} | host score: {local} | \
         remote scores: {remote_scores:?} | remote_pose_seen: {seen}"
    );

    if rendered < frames.saturating_div(2).max(30) as u64 {
        bail!("[{role}] renderer produced too few frames ({rendered})");
    }
    if auto {
        if guest_score == 0 {
            bail!("[{role}] tag score never incremented (proximity mechanic failed)");
        }
        if seen == 0 {
            bail!("[{role}] guest never observed the remote avatar");
        }
    }

    println!("[{role}] ✓ game session complete");
    Ok(())
}

/// Creates a peer connection, retrying until the signaling server is reachable.
fn connect_with_retry(local_id: &str) -> Result<PeerConnection> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match PeerConnection::new(local_id, SIGNAL_SERVER, "127.0.0.1:0") {
            Ok(pc) => return Ok(pc),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(e.context("signaling server did not come up in time"));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

/// Connects to `remote_id`, retrying until the other instance registers.
fn connect_to_peer(store: &mut Store<HostState>, remote_id: &str) -> Result<()> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        let pc = store
            .data_mut()
            .peer_connection_mut()
            .expect("peer connection is set");
        match pc.request_connection(remote_id) {
            Ok(_) => return Ok(()),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(e.context(format!(
                        "could not connect to '{remote_id}' within {CONNECT_TIMEOUT:?}"
                    )));
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
}