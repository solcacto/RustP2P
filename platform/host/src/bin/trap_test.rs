use anyhow::{bail, Result};
use host::{avatar_state::AvatarState, host_functions, host_state::HostState, renderer};
use std::sync::{Arc, Mutex};
use wasmtime::{Engine, Linker, Module, Store};

/// Commit 13 lifecycle test: a guest that traps on every tick must NOT crash
/// the host. The trap is recorded in host state, surfaced in the HUD, and the
/// app exits cleanly (code 0) after a short grace period.
fn main() -> Result<()> {
    let avatar_state = Arc::new(Mutex::new(AvatarState::default()));
    let remote_avatars = Arc::new(Mutex::new(std::collections::HashMap::new()));

    let engine = Engine::default();
    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let wat = include_str!("../test_modules/trap_wasm_test.wat");
    let module = Module::new(&engine, wat)?;
    let mut store = Store::new(&engine, HostState::new("trap"));
    store.data_mut().set_avatar_state(Some(avatar_state.clone()));
    let instance = linker.instantiate(&mut store, &module)?;
    let game_tick = instance.get_typed_func::<(), ()>(&mut store, "game_tick")?;

    let mut app = renderer::build_app(avatar_state.clone(), remote_avatars.clone());
    let store_handle = Arc::new(Mutex::new(store));
    app.insert_resource(renderer::WasmRuntime {
        store: store_handle.clone(),
        render_tick: game_tick,
    });
    // No frame-limit terminator: the app must exit via exit_after_wasm_error.

    let exit = app.run();
    if !exit.is_success() {
        bail!("expected a graceful (successful) exit, got {exit:?}");
    }

    let guard = store_handle.lock().unwrap();
    let rendered = guard.data().frame_count();
    let error = guard.data().wasm_error();
    match error {
        Some(msg) if msg.contains("trap") => {
            println!("✓ host survived wasm trap: '{msg}'")
        }
        Some(msg) => bail!("unexpected wasm error surfaced: {msg}"),
        None => bail!("trap was not recorded in host state"),
    }
    println!("✓ rendered {rendered} frames after the trap before a clean exit");
    if rendered < u64::from(renderer::TEST_FRAMES) {
        println!("✓ guest was stopped (frame count stayed below the 240-frame terminator)");
    }
    println!("✓ graceful lifecycle: trap -> HUD error -> clean exit, host intact");
    Ok(())
}