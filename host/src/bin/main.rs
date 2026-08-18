use anyhow::Result;
use host::host_functions;
use host::host_state::HostState;
use wasmtime::{Engine, Linker, Module, Store};

fn main() -> Result<()> {
    let engine = Engine::default();
    let mut store = Store::new(&engine, HostState::new("test-instance"));

    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;

    let wat = include_str!("../test_module.wat");
    let module = Module::new(&engine, wat)?;

    let instance = linker.instantiate(&mut store, &module)?;
    let run_test = instance.get_typed_func::<(), ()>(&mut store, "run_test")?;
    run_test.call(&mut store, ())?;

    let counter = store.data().counter();
    println!("Counter after test: {counter}");
    assert_eq!(counter, 5);
    Ok(())
}