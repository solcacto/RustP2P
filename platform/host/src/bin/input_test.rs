use anyhow::Result;
use host::host_functions;
use host::host_state::HostState;
use host::input_poller::poll_mock_input;
use wasmtime::{Engine, Linker, Module, Store};

fn main() -> Result<()> {
    let engine = Engine::default();
    let mut store = Store::new(&engine, HostState::new("input-test"));

    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;

    let wat = include_str!("../test_modules/input_test.wat");
    let module = Module::new(&engine, wat)?;
    let instance = linker.instantiate(&mut store, &module)?;
    let process_input = instance.get_typed_func::<(), i32>(&mut store, "process_input")?;

    let duration = std::time::Duration::from_millis(1500);
    let start = std::time::Instant::now();
    let mut frame = 0u64;

    while start.elapsed() < duration {
        let prev = *store.data().input();
        store.data_mut().set_input(poll_mock_input(frame));

        if store.data().input().move_up == 1 && prev.move_up == 0 {
            println!("Key pressed: move_up (frame {frame})");
        }
        if store.data().input().move_right == 1 && prev.move_right == 0 {
            println!("Key pressed: move_right (frame {frame})");
        }
        if store.data().input().action_1 == 1 && prev.action_1 == 0 {
            println!("Key pressed: action_1 (frame {frame})");
        }
        if store.data().input().action_2 == 1 && prev.action_2 == 0 {
            println!("Key pressed: action_2 (frame {frame})");
        }

        let vector = process_input.call(&mut store, ())?;
        if vector != 0 {
            println!("Input detected: movement vector = {vector} (frame {frame})");
        }

        frame += 1;
        std::thread::sleep(std::time::Duration::from_millis(16));
    }

    let state = store.data();
    println!(
        "Final state: frame={frame} up={} down={} left={} right={} action_1={} action_2={}",
        state.input().move_up,
        state.input().move_down,
        state.input().move_left,
        state.input().move_right,
        state.input().action_1,
        state.input().action_2
    );
    Ok(())
}