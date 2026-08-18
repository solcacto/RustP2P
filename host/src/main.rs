use wasmtime::{Engine, Module, Store};

fn main() -> anyhow::Result<()> {
    let engine = Engine::default();
    let wasm_path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/guest.wasm");
    let module = Module::from_file(&engine, wasm_path)?;

    let mut store = Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;

    let init = instance.get_typed_func::<(), i32>(&mut store, "init")?;
    let value = init.call(&mut store, ())?;

    assert_eq!(value, 42);
    println!("Wasm module loaded successfully");
    Ok(())
}
