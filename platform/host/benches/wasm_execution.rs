//! Criterion benchmarks for the Wasm guest: execution + instantiation.
//!
//! The guest is the committed Chase Tag module (`platform/guest/guest.wasm`),
//! loaded through the real host-function linker.

use criterion::{criterion_group, criterion_main, Criterion};
use host::{host_functions, host_state::HostState};
use wasmtime::{Engine, Linker, Module, Store};

fn bench_wasm_game_tick(c: &mut Criterion) {
    let engine = Engine::default();
    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker).unwrap();
    let wasm = include_bytes!("../../guest/guest.wasm").to_vec();
    let module = Module::new(&engine, &wasm).unwrap();
    let mut store = Store::new(&engine, HostState::new("bench"));
    let instance = linker.instantiate(&mut store, &module).unwrap();
    let tick = instance.get_typed_func::<(), ()>(&mut store, "game_tick").unwrap();

    c.bench_function("wasm_game_tick", |b| {
        b.iter(|| tick.call(&mut store, ()).unwrap())
    });
}

fn bench_wasm_instantiation(c: &mut Criterion) {
    let engine = Engine::default();
    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker).unwrap();
    let wasm = include_bytes!("../../guest/guest.wasm").to_vec();

    c.bench_function("wasm_instantiation", |b| {
        b.iter(|| {
            let module = Module::new(&engine, &wasm).unwrap();
            let mut store = Store::new(&engine, HostState::new("bench"));
            let _ = linker.instantiate(&mut store, &module).unwrap();
        })
    });
}

criterion_group!(benches, bench_wasm_game_tick, bench_wasm_instantiation);
criterion_main!(benches);
