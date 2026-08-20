use anyhow::{bail, Result};
use host::{
    avatar_state::{AvatarPose, AvatarState, WorldObject},
    host_functions,
    host_state::HostState,
    renderer,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wasmtime::{Engine, Linker, Module, Store};

/// Integration test for Commit 8: the Wasm guest drives a 3D scene through the
/// `update_avatar_transform` host function while Bevy renders each frame.
fn main() -> Result<()> {
    // Shared pose bridge between the Wasm host function and the Bevy scene.
    let avatar_state = Arc::new(Mutex::new(AvatarState::default()));
    let remote_avatars: Arc<Mutex<HashMap<String, AvatarPose>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let world_objects: Arc<Mutex<Vec<WorldObject>>> = Arc::new(Mutex::new(Vec::new()));

    // Instantiate the Wasm render module.
    let engine = Engine::default();
    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let wat = include_str!("../test_modules/render_wasm_test.wat");
    let module = Module::new(&engine, wat)?;
    let mut store = Store::new(&engine, HostState::new("render"));
    store.data_mut().set_avatar_state(Some(avatar_state.clone()));
    let instance = linker.instantiate(&mut store, &module)?;
    let render_tick = instance.get_typed_func::<(), ()>(&mut store, "render_tick")?;

    // Build the Bevy renderer and give it the Wasm runtime so one guest tick
    // runs per rendered frame.
    let mut app =
        renderer::build_app(avatar_state.clone(), remote_avatars.clone(), world_objects.clone());
    let store_handle = Arc::new(Mutex::new(store));
    app.insert_resource(renderer::WasmRuntime {
        store: store_handle.clone(),
        render_tick,
    });
    renderer::add_test_terminator(&mut app);

    // Bevy 0.14's winit runner owns the main loop; `run` blocks until the app
    // self-terminates after ~3 seconds of rendering.
    let exit = app.run();
    if !exit.is_success() {
        bail!("renderer exited with an error: {exit:?}");
    }

    // The guest frame counter is advanced by the render tick each rendered
    // frame, so it directly proves how many frames the window produced.
    let rendered_frames = store_handle.lock().unwrap().data().frame_count();
    println!("Rendered {rendered_frames} frames");
    if rendered_frames < 100 {
        bail!("renderer produced too few frames ({rendered_frames}); window likely failed");
    }

    // Verify the Wasm guest actually moved the avatar through the host
    // function into the scene graph.
    let pose = *avatar_state.lock().unwrap();
    let travelled = (pose.x * pose.x + pose.z * pose.z).sqrt();
    println!(
        "Final avatar pose: x={:.3} y={:.3} z={:.3} rot_y={:.3} (travelled {:.3} units)",
        pose.x, pose.y, pose.z, pose.rot_y, travelled
    );
    if travelled < 0.1 && pose.rot_y.abs() < 0.1 {
        bail!("avatar never moved: wasm render_tick did not drive the scene");
    }

    println!("✓ 3D scene rendered: standard avatar loaded and animated by Wasm logic");
    Ok(())
}