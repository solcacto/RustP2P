use anyhow::{bail, Result};
use host::{
    avatar_state::{AvatarPose, AvatarState, WorldObject},
    host_functions,
    host_state::HostState,
    net_link::NetLink,
    peer_connection::PeerConnection,
    renderer,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wasmtime::{Engine, Linker, Module, Store};

const SIGNAL_SERVER: &str = "127.0.0.1:9001";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Integration test for Commit 9: two processes (roles A and B) each drive a
/// Universal Avatar through the same Wasm module, broadcast their pose over the
/// P2P link, and render the remote peer's avatar in a second Bevy window.
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let role = args
        .iter()
        .position(|a| a == "--role")
        .map(|i| args[i + 1].clone())
        .unwrap_or_else(|| "A".to_string());
    if role != "A" && role != "B" {
        bail!("usage: multiplayer_test --role A|B (default A)");
    }
    let local_id = format!("Peer{role}");
    let remote_id = if role == "A" { "PeerB" } else { "PeerA" };
    let axis: u8 = if role == "A" { 0 } else { 1 };

    // Shared pose bridges between the Wasm host functions, the network poll,
    // and the Bevy scene graph.
    let avatar_state = Arc::new(Mutex::new(AvatarState::default()));
    let remote_avatars: Arc<Mutex<HashMap<String, AvatarPose>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let world_objects: Arc<Mutex<Vec<WorldObject>>> = Arc::new(Mutex::new(Vec::new()));

    // Instantiate the Wasm multiplayer module.
    let engine = Engine::default();
    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let wat = include_str!("../test_modules/multiplayer_wasm_test.wat");
    let module = Module::new(&engine, wat)?;
    let mut store = Store::new(&engine, HostState::new(local_id.clone()));
    store
        .data_mut()
        .set_avatar_state(Some(avatar_state.clone()));
    store.data_mut().set_remote_avatars(remote_avatars.clone());
    store.data_mut().set_world_objects(world_objects.clone());
    store.data_mut().set_movement_axis(axis);
    store
        .data_mut()
        .set_peer_connection(Some(connect_with_retry(&local_id)?));
    let instance = linker.instantiate(&mut store, &module)?;
    let multiplayer_tick = instance.get_typed_func::<(), ()>(&mut store, "multiplayer_tick")?;

    // Block until the other instance is registered and a P2P link is up.
    println!("[{role}] connecting to {remote_id} via signaling server...");
    connect_to_peer(&mut store, remote_id)?;
    println!("[{role}] connected to {remote_id}");

    // Build the Bevy renderer: one multiplayer tick per rendered frame.
    let mut app =
        renderer::build_app(avatar_state.clone(), remote_avatars.clone(), world_objects.clone());
    let store_handle = Arc::new(Mutex::new(store));
    app.insert_resource(renderer::WasmRuntime {
        store: store_handle.clone(),
        render_tick: multiplayer_tick,
    });
    renderer::add_test_terminator(&mut app);

    let exit = app.run();
    if !exit.is_success() {
        bail!("renderer exited with an error: {exit:?}");
    }

    // Graceful shutdown: close the signaling WebSocket before the process exits.
    let mut guard = store_handle.lock().unwrap();
    if let Some(pc) = guard.data_mut().peer_connection_mut() {
        pc.shutdown();
    }
    drop(guard);

    // The renderer self-terminates after a few seconds of rendering.
    let rendered = store_handle.lock().unwrap().data().frame_count();
    println!("[{role}] rendered {rendered} frames");
    if rendered < 100 {
        bail!("[{role}] renderer produced too few frames ({rendered})");
    }

    // Verify the remote peer's pose arrived over the wire AND that the Wasm
    // guest successfully read it back through get_remote_avatar_pose.
    let mut guard = store_handle.lock().unwrap();
    let seen = instance.get_typed_func::<(), i32>(&mut *guard, "remote_pose_seen")?;
    let seen_val = seen.call(&mut *guard, ())?;
    let pose = remote_avatars.lock().unwrap().get(remote_id).copied();
    match (seen_val, pose) {
        (1, Some(p)) => println!(
            "[{role}] ✓ remote '{remote_id}' pose synced: x={:.3} y={:.3} z={:.3} rot_y={:.3}",
            p.x, p.y, p.z, p.rot_y
        ),
        (_, Some(_)) => bail!("[{role}] remote pose received but the Wasm guest could not read it"),
        _ => bail!("[{role}] never received a pose from '{remote_id}'"),
    }

    println!("[{role}] ✓ multiplayer visual sync complete");
    Ok(())
}

/// Creates a peer connection, retrying until the signaling server is reachable.
fn connect_with_retry(local_id: &str) -> Result<NetLink> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match PeerConnection::new(local_id, SIGNAL_SERVER, "127.0.0.1:0")
            .map(|pc| NetLink::Udp(Box::new(pc)))
        {
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
