use anyhow::{bail, Context, Result};
use host::{
    avatar_state::{AvatarPose, AvatarState},
    avatar_standard,
    cosmetic,
    host_functions,
    host_state::HostState,
    manifest::GameManifest,
    peer_connection::PeerConnection,
    renderer,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wasmtime::{Engine, Linker, Module, Store};

const SIGNAL_SERVER: &str = "127.0.0.1:9001";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const GAME_PACKAGE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../guest");
const AVATAR_ASSET_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");

/// Commit 17: loads a game *package* — a `game_manifest.json` plus its Wasm —
/// validates the manifest, refuses to load if the pinned SHA-256 doesn't match,
/// then runs the Chase/Tag session with real keyboard input and a score HUD.
/// With `--cid <CID>` the package is fetched from IPFS instead of the local
/// guest directory.
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let role = args
        .iter()
        .position(|a| a == "--role")
        .map(|i| args[i + 1].clone())
        .unwrap_or_else(|| "A".to_string());
    if role != "A" && role != "B" {
        bail!("usage: play_game --role A|B [--cid <CID> | --package <dir>] [--ipfs <api>] [--avatar <path>] [--cosmetic <manifest>] [--auto] [--frames N] [--no-exit]");
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
    let cid = args
        .iter()
        .position(|a| a == "--cid")
        .and_then(|i| args.get(i + 1))
        .cloned();
    // Load a game from an arbitrary local package directory instead of the
    // default guest dir (used for SDK-scaffolded games).
    let package_arg = args
        .iter()
        .position(|a| a == "--package")
        .and_then(|i| args.get(i + 1))
        .cloned();
    // IPFS node API to use for downloads (defaults to the local Kubo node).
    let ipfs_api = args
        .iter()
        .position(|a| a == "--ipfs")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| host::ipfs::IPFS_API.to_string());
    // Avatar asset path, relative to the host asset folder.
    let avatar_path = args
        .iter()
        .position(|a| a == "--avatar")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "avatar_standard.glb".to_string());
    // Cosmetic slots, repeatable: --cosmetic <manifest path> (relative to the
    // host asset folder), e.g. cosmetics/golden_sword/cosmetic_manifest.json.
    // Only signature-verified cosmetics are slotted; invalid ones are silently
    // not rendered.
    let cosmetic_args: Vec<String> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == "--cosmetic")
        .filter_map(|(i, _)| args.get(i + 1))
        .cloned()
        .collect();

    // Shared pose bridges between the Wasm guest, the network poll, and Bevy.
    let avatar_state = Arc::new(Mutex::new(AvatarState::default()));
    let remote_avatars: Arc<Mutex<HashMap<String, AvatarPose>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Determine the game package location: from IPFS by CID, an explicit
    // --package directory, or the local guest directory.
    let (package_dir, manifest_path): (std::path::PathBuf, std::path::PathBuf) = match &cid {
        Some(cid) => {
            let ipfs = host::ipfs::IpfsClient::new(&ipfs_api);
            let game_dir = fetch_game_from_ipfs(&ipfs, cid)?;
            (game_dir.clone(), game_dir.join("game_manifest.json"))
        }
        None => {
            let dir = package_arg
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from(GAME_PACKAGE_DIR));
            let manifest = dir.join("game_manifest.json");
            if !manifest.exists() {
                bail!(
                    "no game_manifest.json in {} — use --cid, --package, or build the guest",
                    dir.display()
                );
            }
            (dir, manifest)
        }
    };

    // Load and validate the game package manifest, then load the Wasm it pins
    // and verify the artifact's hash before trusting a single byte.
    let manifest = GameManifest::from_path(&manifest_path)?;
    println!(
        "[{role}] game: {} v{} by {} (mode={:?}, max_players={})",
        manifest.name, manifest.version, manifest.author, manifest.mode, manifest.max_players
    );
    let wasm_path = package_dir.join(&manifest.wasm_entry);
    let wasm_bytes = std::fs::read(&wasm_path).with_context(|| {
        format!(
            "{} not found in game package {} — build the guest first:\n  \
             cargo build -p guest --target wasm32-unknown-unknown --release && \
             cp target/wasm32-unknown-unknown/release/guest.wasm platform/guest/guest.wasm",
            manifest.wasm_entry,
            package_dir.display()
        )
    })?;
    manifest.verify_wasm(&manifest.wasm_entry, &wasm_bytes)?;
    manifest.verify_publisher(&wasm_bytes)?;
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
    let mut cosmetics: Vec<(renderer::AttachmentPoint, renderer::CosmeticSlot)> = Vec::new();
    for manifest_rel in &cosmetic_args {
        match cosmetic::verify_package(AVATAR_ASSET_DIR, manifest_rel) {
            Ok(pkg) => {
                let Some(point) = renderer::AttachmentPoint::parse(&pkg.manifest.attachment_point)
                else {
                    println!(
                        "[{role}] cosmetic '{}' rejected: unknown attachment point '{}'",
                        pkg.manifest.item_id, pkg.manifest.attachment_point
                    );
                    continue;
                };
                cosmetics.push((
                    point,
                    renderer::CosmeticSlot {
                        item_id: pkg.manifest.item_id.clone(),
                        mesh_asset_path: pkg.mesh_asset_path.clone(),
                    },
                ));
                println!(
                    "[{role}] cosmetic '{}' verified (signature OK) -> {}",
                    pkg.manifest.item_id, pkg.manifest.attachment_point
                );
            }
            Err(e) => {
                // Signature invalid (tampered manifest or mesh): the cosmetic
                // is silently not rendered — the host just logs the rejection.
                println!("[{role}] cosmetic '{manifest_rel}' rejected (not rendered): {e:#}");
            }
        }
    }
    let cosmetic_desc: Vec<String> = cosmetics
        .iter()
        .map(|(p, s)| format!("{}:{}", p.as_str(), s.item_id))
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

    // Post-run verification. Guest diagnostics exports are optional (the SDK
    // games don't emit them); host-side state is authoritative.
    let rendered = store_handle.lock().unwrap().data().frame_count();
    println!("[{role}] rendered {rendered} frames");
    let mut guard = store_handle.lock().unwrap();
    let guest_score = instance
        .get_typed_func::<(), i32>(&mut *guard, "get_tag_score")
        .ok()
        .and_then(|f| f.call(&mut *guard, ()).ok())
        .unwrap_or(0);
    let seen = instance
        .get_typed_func::<(), i32>(&mut *guard, "remote_pose_seen")
        .ok()
        .and_then(|f| f.call(&mut *guard, ()).ok())
        .unwrap_or(0);
    let local = guard.data().tag_score();
    let remote_pose_count = guard.data().remote_avatars().lock().unwrap().len();
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
         remote scores: {remote_scores:?} | remote_pose_seen: {seen} (peers: {remote_pose_count})"
    );

    if rendered < frames.saturating_div(2).max(30) as u64 {
        bail!("[{role}] renderer produced too few frames ({rendered})");
    }
    if auto {
        if local == 0 && guest_score == 0 {
            bail!("[{role}] tag score never incremented (proximity mechanic failed)");
        }
        if seen == 0 && remote_pose_count == 0 {
            bail!("[{role}] guest never observed the remote avatar");
        }
    }

    println!("[{role}] ✓ game session complete");
    Ok(())
}

/// Fetches a game bundle from IPFS by CID, extracts it into `games/<cid>/`,
/// pins it (so this node seeds the game for the swarm), and returns the
/// extracted package directory.
fn fetch_game_from_ipfs(ipfs: &host::ipfs::IpfsClient, cid: &str) -> Result<std::path::PathBuf> {
    let games_dir = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/games"));
    let game_dir = games_dir.join(cid);
    if game_dir.join("game_manifest.json").exists() {
        println!("[{cid}] already downloaded; using local copy at {}", game_dir.display());
        return Ok(game_dir);
    }

    println!("[{cid}] fetching bundle from IPFS...");
    let tar_bytes = ipfs.cat(cid)?;
    println!("[{cid}] fetched {} bytes", tar_bytes.len());

    // Remove any stale extraction and unpack fresh.
    let _ = std::fs::remove_dir_all(&game_dir);
    host::ipfs::extract(&tar_bytes, &game_dir)?;
    println!("[{cid}] extracted game package to {}", game_dir.display());

    // Become a seeder: pin the bundle so this node serves it to the swarm.
    match ipfs.pin(cid) {
        Ok(()) => println!("[{cid}] pinned — this node is now a seeder for the game"),
        Err(e) => println!("[{cid}] warning: could not pin bundle: {e}"),
    }
    Ok(game_dir)
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