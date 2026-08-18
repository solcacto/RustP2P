use anyhow::Result;
use host::host_functions;
use host::host_state::HostState;
use wasmtime::{Engine, Linker, Module, Store};

fn main() -> Result<()> {
    let engine = Engine::default();
    let mut store = Store::new(&engine, HostState::new("game-loop"));

    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;

    let wat = include_str!("../test_modules/game_loop_test.wat");
    let module = Module::new(&engine, wat)?;
    let instance = linker.instantiate(&mut store, &module)?;

    let game_tick = instance.get_typed_func::<(), u64>(&mut store, "game_tick")?;

    let duration = std::time::Duration::from_secs(1);
    let start = std::time::Instant::now();

    while start.elapsed() < duration {
        let frame = game_tick.call(&mut store, ())?;
        debug_assert_eq!(frame, store.data().frame_count());
        println!(
            "Frame: {} | Delta: {:.4}s",
            store.data().frame_count(),
            store.data().delta_time()
        );
        std::thread::sleep(std::time::Duration::from_millis(16));
    }

    let memory = instance.get_memory(&mut store, "memory");
    if let Some(memory) = memory {
        let mut buf = [0u8; 8];
        memory.read(&store, 0, &mut buf)?;
        let stored_delta = f64::from_le_bytes(buf);
        let host_delta = store.data().delta_time();
        debug_assert!((stored_delta - host_delta).abs() < 1e-9);
        println!("Wasm-stored delta_time verified: {stored_delta:.4}s");
    }

    println!("Final frame count: {}", store.data().frame_count());
    Ok(())
}
