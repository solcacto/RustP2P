use anyhow::{bail, Context, Result};
use host::{
    avatar_standard,
    avatar_state::{AvatarPose, AvatarState, WorldObject},
    cosmetic,
    deployment::{DeploymentConfig, NetworkConfig},
    host_functions,
    host_state::HostState,
    manifest::GameManifest,
    net_link::NetLink,
    renderer,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wasmtime::{Engine, Linker, Module, Store};
use webrtc::ice_transport::ice_server::RTCIceServer;

const DEFAULT_SIGNAL_SERVER: &str = "127.0.0.1:9001";
const CONFIG_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/config.toml");
const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);
const GAME_PACKAGE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../guest");
const AVATAR_ASSET_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");

/// The directory containing this executable. Shipped bundles carry the game
/// binaries, `config.toml`, `assets/`, and the `games/` cache next to the
/// executable, so runtime resolution lets a standalone build work on any
/// machine instead of relying on compile-time workspace paths.
fn exe_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

/// The avatar/cosmetic asset directory: prefer the sibling `assets/` folder of
/// this executable, falling back to the compile-time workspace path (used when
/// run from `cargo run`).
fn asset_dir() -> std::path::PathBuf {
    let sibling = exe_dir().join("assets");
    if sibling.join("avatar_standard.glb").exists() {
        sibling
    } else {
        std::path::PathBuf::from(AVATAR_ASSET_DIR)
    }
}

/// The downloaded-game cache: prefer the sibling `games/` folder of this
/// executable when writable (e.g. unpacked from a DMG/zip), otherwise a
/// user-writable cache directory (a mounted DMG volume is read-only even
/// though a bundled `games/` folder exists).
fn games_dir() -> std::path::PathBuf {
    let sibling = exe_dir().join("games");
    if writable_dir(&sibling) {
        sibling
    } else {
        user_cache_dir()
    }
}

/// Creates `dir` (if needed) and confirms it can actually hold a file, so a
/// folder that merely *exists* on a read-only volume is not mistaken for a
/// usable cache.
fn writable_dir(dir: &std::path::Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".write_test");
    match std::fs::File::create(&probe) {
        Ok(_) => std::fs::remove_file(&probe).is_ok(),
        Err(_) => false,
    }
}

/// A writable, per-user cache directory for downloaded game bundles.
fn user_cache_dir() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        if let Ok(base) = std::env::var("LOCALAPPDATA") {
            return std::path::PathBuf::from(base).join("RustP2P").join("games");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return std::path::PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("RustP2P")
            .join("games");
    }
    std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/games"))
}

/// The deployment config: a sibling `config.toml` overrides the baked-in
/// workspace path (which is itself preferred over the local-signal default).
fn config_path() -> std::path::PathBuf {
    let sibling = exe_dir().join("config.toml");
    if sibling.exists() {
        sibling
    } else {
        std::path::PathBuf::from(CONFIG_PATH)
    }
}

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
        bail!("usage: play_game --role A|B [--cid <CID> | --package <dir>] [--ipfs <api>] [--avatar <path>] [--cosmetic <manifest>] [--auto] [--frames N] [--no-exit] [--solo]");
    }
    // Seamless lobby: --lobby <cid> auto-discovers a host or becomes one.
    // Generates a unique peer id so multiple lobby members don't collide.
    let lobby_cid = args
        .iter()
        .position(|a| a == "--lobby")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let (local_id, remote_id, mut axis) = if lobby_cid.is_some() {
        let uniq = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
            % 9000
            + 1000) as u32;
        (format!("Peer{uniq}"), String::new(), 0u8)
    } else {
        let local = format!("Peer{role}");
        let remote = if role == "A" { "PeerB" } else { "PeerA" }.to_string();
        let ax: u8 = if role == "A" { 0 } else { 1 };
        (local, remote, ax)
    };
    let auto = args.iter().any(|a| a == "--auto");
    let no_exit = args.iter().any(|a| a == "--no-exit");
    let solo = args.iter().any(|a| a == "--solo");
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
    let world_objects: Arc<Mutex<Vec<WorldObject>>> = Arc::new(Mutex::new(Vec::new()));

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
    let avatar_file = asset_dir().join(&avatar_path);
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
        match cosmetic::verify_package(asset_dir(), manifest_rel) {
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
    println!(
        "[{role}] cosmetics: {}",
        if cosmetic_desc.is_empty() {
            "none".to_string()
        } else {
            cosmetic_desc.join(", ")
        }
    );

    let mut config = wasmtime::Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config)?;
    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let module = Module::new(&engine, wasm_bytes)?;
    let mut store = Store::new(&engine, HostState::new(local_id.clone()));
    store.set_fuel(host::host_state::MAX_FUEL_PER_TICK)?;
    store.limiter(|state| state as &mut dyn wasmtime::ResourceLimiter);
    // Bind peer_id → ed25519 identity so poses/scores can be authenticated.
    store.data_mut().generate_identity();
    println!(
        "[identity] {} → {}",
        store.data().peer_id(),
        store.data().owner_pubkey()
    );
    store
        .data_mut()
        .set_avatar_state(Some(avatar_state.clone()));
    store.data_mut().set_remote_avatars(remote_avatars.clone());
    store.data_mut().set_world_objects(world_objects.clone());
    store.data_mut().set_avatar_path(avatar_path.clone());
    if solo {
        // Solo mode: no network link at all. The guest's network calls no-op
        // through the host (they return "no peer" gracefully), so a solo game
        // can still use input, chunk edits, and the HUD.
        println!("[{role}] solo mode — no network link (play as a single player)");
    } else if let Some(ref lobby) = lobby_cid {
        // Seamless lobby: connect, look for an existing host for this game,
        // join if a slot is free, otherwise host and enter immediately.
        // Never blocks the game on another player.
        let mut link = connect_with_lobby(&local_id, lobby)?;
        let max_players = manifest.max_players.max(1) as usize;
        let (r, ax) = join_or_host(&mut link, lobby, max_players);
        axis = ax;
        println!("[lobby:{lobby}] entering as {r} (axis {ax})");
        store.data_mut().set_peer_connection(Some(link));
    } else {
        store
            .data_mut()
            .set_peer_connection(Some(connect_with_retry(&local_id)?));
    }
    store.data_mut().set_movement_axis(axis);
    let instance = linker.instantiate(&mut store, &module)?;
    let game_tick = instance.get_typed_func::<(), ()>(&mut store, "game_tick")?;
    println!(
        "[{role}] verified + loaded {} ({wasm_size} bytes, hash OK)",
        manifest.wasm_entry
    );

    // Classic host/join path blocks until the peer arrives. Lobby mode skips
    // this entirely (late joiners are accepted via per-frame poll_signaling).
    if !solo && lobby_cid.is_none() {
        println!("[{role}] connecting to {remote_id} via signaling server...");
        connect_to_peer(&mut store, &remote_id)?;
        println!("[{role}] connected to {remote_id}");
    }

    // Chunk ownership: on joining, claim the spawn chunk plus the one to +X.
    // ("I am hosting chunks X,Y and X+1,Y.")
    store
        .data_mut()
        .claim_chunk_region(host::chunk::ChunkCoord { x: 0, z: 0 }, (2, 1))?;
    println!("[{role}] hosting chunks (0,0) and (1,0) (registered in the chunk DHT)");

    // Build the Bevy renderer: one guest `game_tick` per rendered frame.
    let mut app =
        renderer::build_app(avatar_state.clone(), remote_avatars.clone(), world_objects.clone());
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

    // Graceful shutdown: publish this peer's chunk states to IPFS (so the
    // world's "ruins" survive its departure), then close the signaling
    // WebSocket and UDP socket before the process exits.
    let mut guard = store_handle.lock().unwrap();
    match guard.data_mut().publish_chunk_states() {
        Ok(n) if n > 0 => println!("[{role}] published {n} chunk state(s) to IPFS"),
        Ok(_) => {}
        Err(e) => println!("[{role}] warning: chunk state publish failed: {e}"),
    }
    if let Some(pc) = guard.data_mut().peer_connection_mut() {
        pc.shutdown();
        println!("[{role}] signaling connection closed cleanly");
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
        if !solo && seen == 0 && remote_pose_count == 0 {
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
    let games_dir = games_dir();
    let game_dir = games_dir.join(cid);
    if game_dir.join("game_manifest.json").exists() {
        println!(
            "[{cid}] already downloaded; using local copy at {}",
            game_dir.display()
        );
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

/// Lobby-aware connect: same as [`connect_with_retry`] but tags the
/// registration with the game CID so other lobby members can discover us.
fn connect_with_lobby(local_id: &str, game_cid: &str) -> Result<NetLink> {
    let (signal, ice) = match DeploymentConfig::load(&config_path()) {
        Ok(cfg) => (
            cfg.network.signaling_url.clone(),
            ice_servers_from_config(&cfg.network),
        ),
        Err(_) => (DEFAULT_SIGNAL_SERVER.to_string(), Vec::new()),
    };
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match NetLink::new_with_game(local_id, &signal, "127.0.0.1:0", ice.clone(), game_cid) {
            Ok(link) => return Ok(link),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(e.context("signaling server did not come up in time"));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

/// Seamless join-or-host: if a lobby host exists for this game and has a free
/// slot, connect to it (role B); otherwise become the host (role A) and enter
/// immediately — never blocks on another player.
fn join_or_host(link: &mut NetLink, game_cid: &str, max_players: usize) -> (String, u8) {
    let peers = link.list_peers(game_cid).unwrap_or_default();
    // If the lobby is at max capacity, start a parallel session as host
    // rather than erroring.
    if peers.len() >= max_players.max(1) {
        println!("[lobby:{game_cid}] session full ({} peers) — starting parallel session", peers.len());
        return ("A".to_string(), 0);
    }
    if let Some((host_id, _addr, _game)) = peers.into_iter().next() {
        println!("[lobby:{game_cid}] found host '{host_id}' — joining...");
        match link.try_connect(&host_id, Duration::from_secs(3)) {
            Ok(_) => {
                println!("[lobby:{game_cid}] connected to '{host_id}'");
                return ("B".to_string(), 1);
            }
            Err(e) => {
                println!("[lobby:{game_cid}] join failed ({e:#}) — hosting instead");
            }
        }
    } else {
        println!("[lobby:{game_cid}] no host found — becoming host");
    }
    ("A".to_string(), 0)
}

/// Creates a network link (WebRTC through the deployed Worker, or UDP against
/// a local signaling server), retrying until the signaling server is reachable.
/// The transport is chosen by the `[network]` section of `config.toml`:
/// a full `ws://`/`wss://` URL selects WebRTC, anything else falls back to the
/// classic UDP path (defaulting to `127.0.0.1:9001` for local development).
fn connect_with_retry(local_id: &str) -> Result<NetLink> {
    let (signal, ice) = match DeploymentConfig::load(&config_path()) {
        Ok(cfg) => {
            println!(
                "[net] config.toml: signaling {} ({} stun, {} turn)",
                cfg.network.signaling_url,
                cfg.network.stun_servers.len(),
                cfg.network.turn_servers.len(),
            );
            (
                cfg.network.signaling_url.clone(),
                ice_servers_from_config(&cfg.network),
            )
        }
        Err(e) => {
            println!("[net] no config.toml ({e:#}); using local signaling server {DEFAULT_SIGNAL_SERVER}");
            (DEFAULT_SIGNAL_SERVER.to_string(), Vec::new())
        }
    };
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match NetLink::new(local_id, &signal, "127.0.0.1:0", ice.clone()) {
            Ok(link) => return Ok(link),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(e.context("signaling server did not come up in time"));
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

/// Converts the `[network]` config section into WebRTC ICE servers.
fn ice_servers_from_config(net: &NetworkConfig) -> Vec<RTCIceServer> {
    let mut servers: Vec<RTCIceServer> = net
        .stun_servers
        .iter()
        .map(|url| RTCIceServer {
            urls: vec![url.clone()],
            ..Default::default()
        })
        .collect();
    if !net.turn_servers.is_empty() {
        servers.push(RTCIceServer {
            urls: net.turn_servers.clone(),
            username: net.turn_username.clone(),
            credential: net.turn_password.clone(),
        });
    }
    if servers.is_empty() {
        servers.push(RTCIceServer {
            urls: vec![host::webrtc_connection::DEFAULT_STUN_URL.to_string()],
            ..Default::default()
        });
    }
    servers
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
