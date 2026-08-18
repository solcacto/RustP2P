use anyhow::{bail, Result};
use host::host_functions;
use host::host_state::HostState;
use wasmtime::{Config, Engine, Linker, Module, Store};

const TEST_MODULES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/test_modules");

fn load_test_module(engine: &Engine, file: &str) -> Result<Module> {
    let wat = std::fs::read_to_string(format!("{TEST_MODULES_DIR}/{file}"))?;
    Module::new(engine, wat)
}

fn new_store(engine: &Engine, name: &str) -> Store<HostState> {
    Store::new(engine, HostState::new(name))
}

fn verify_host_state(store: &Store<HostState>) -> Result<()> {
    let state = store.data();
    if state.counter() == 0 {
        println!(
            "✓ Host state remains secure (counter={}, name={})",
            state.counter(),
            state.name()
        );
        Ok(())
    } else {
        bail!(
            "✗ Host state corrupted (counter={}, name={})",
            state.counter(),
            state.name()
        )
    }
}

fn test_out_of_bounds() -> Result<()> {
    let engine = Engine::default();
    let module = load_test_module(&engine, "malicious_out_of_bounds.wat")?;

    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let mut store = new_store(&engine, "out-of-bounds");

    let instance = linker.instantiate(&mut store, &module)?;
    match instance.get_func(&mut store, "attempt_out_of_bounds") {
        Some(func) => match func.call(&mut store, &[], &mut []) {
            Ok(_) => bail!("✗ Security breach - out-of-bounds write succeeded!"),
            Err(trap) => println!("✓ Out-of-bounds access blocked: {trap}"),
        },
        None => println!("✓ Out-of-bounds function not found (safe)"),
    }

    verify_host_state(&store)
}

fn test_undefined_import() -> Result<()> {
    let engine = Engine::default();
    let module = load_test_module(&engine, "malicious_undefined_import.wat")?;

    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let mut store = new_store(&engine, "undefined-import");

    match linker.instantiate(&mut store, &module) {
        Ok(_) => bail!("✗ Security breach - undefined import was accepted!"),
        Err(e) => println!("✓ Undefined import blocked: {e:#}"),
    }

    verify_host_state(&store)
}

fn test_infinite_loop() -> Result<()> {
    let mut config = Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config)?;
    let module = load_test_module(&engine, "malicious_infinite_loop.wat")?;

    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let mut store = new_store(&engine, "infinite-loop");
    store.set_fuel(10_000)?;

    let instance = linker.instantiate(&mut store, &module)?;
    match instance.get_func(&mut store, "attempt_infinite_loop") {
        Some(func) => match func.call(&mut store, &[], &mut []) {
            Ok(_) => bail!("✗ Security breach - infinite loop was not interrupted!"),
            Err(trap) => println!("✓ Infinite loop blocked (fuel exhausted): {trap}"),
        },
        None => println!("✓ Infinite loop function not found (safe)"),
    }

    verify_host_state(&store)
}

fn main() -> Result<()> {
    test_out_of_bounds()?;
    test_undefined_import()?;
    test_infinite_loop()?;
    println!("All security tests passed.");
    Ok(())
}
