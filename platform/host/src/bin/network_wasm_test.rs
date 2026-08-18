use anyhow::{anyhow, bail, Result};
use host::host_functions;
use host::host_state::HostState;
use host::peer_connection::PeerConnection;
use std::thread;
use std::time::Duration;
use wasmtime::{Engine, Instance, Linker, Module, Store};

const SIGNAL_SERVER: &str = "127.0.0.1:9001";

fn setup_peer(peer_id: &str) -> Result<(Store<HostState>, Instance)> {
    let engine = Engine::default();
    let mut store = Store::new(&engine, HostState::new(peer_id));

    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let wat = include_str!("../test_modules/network_wasm_test.wat");
    let module = Module::new(&engine, wat)?;
    let instance = linker.instantiate(&mut store, &module)?;

    store.data_mut().set_peer_connection(Some(PeerConnection::new(
        peer_id,
        SIGNAL_SERVER,
        "127.0.0.1:0",
    )?));
    Ok((store, instance))
}

fn write_str(instance: &Instance, store: &mut Store<HostState>, offset: usize, s: &str) -> Result<()> {
    let mem = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| anyhow!("no memory export"))?;
    mem.write(&mut *store, offset, s.as_bytes())?;
    Ok(())
}

fn read_mem(instance: &Instance, store: &mut Store<HostState>, offset: usize, len: usize) -> Result<Vec<u8>> {
    let mem = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| anyhow!("no memory export"))?;
    let mut buf = vec![0u8; len];
    mem.read(&mut *store, offset, &mut buf)?;
    Ok(buf)
}

fn poll_and_forward(store: &mut Store<HostState>) {
    let msgs = store.data().peer_connection().unwrap().poll_incoming();
    store.data_mut().incoming_messages_mut().extend(msgs);
}

fn main() -> Result<()> {
    thread::spawn(|| {
        let _ = signaling_server::serve(SIGNAL_SERVER);
    });
    thread::sleep(Duration::from_millis(200));

    let (mut store_a, instance_a) = setup_peer("host-a")?;
    let (mut store_b, instance_b) = setup_peer("host-b")?;

    let b_addr = store_a
        .data_mut()
        .peer_connection_mut()
        .unwrap()
        .request_connection("host-b")?;
    let _a_addr = store_b
        .data_mut()
        .peer_connection_mut()
        .unwrap()
        .wait_for_connection_info("host-a", Duration::from_secs(5))?;
    println!("Host A discovered Host B at {b_addr}");

    // --- Host A sends "WasmPing" ---
    write_str(&instance_a, &mut store_a, 0, "host-b")?;
    write_str(&instance_a, &mut store_a, 100, "WasmPing")?;
    let sent_a = {
        let f = instance_a
            .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store_a, "tick_network")?;
        f.call(&mut store_a, (0, 6, 100, 8))?
    };
    println!("Host A tick returned {sent_a} (expected 0, nothing to receive)");
    if sent_a != 0 {
        bail!("Host A unexpectedly received a message");
    }

    // --- Host B receives "WasmPing", sends "WasmPong!" ---
    poll_and_forward(&mut store_b);
    write_str(&instance_b, &mut store_b, 0, "host-a")?;
    write_str(&instance_b, &mut store_b, 100, "WasmPong!")?;
    let received_b = {
        let f = instance_b
            .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store_b, "tick_network")?;
        f.call(&mut store_b, (0, 6, 100, 9))?
    };
    println!("Host B received message of length {received_b}");
    if received_b != 8 {
        bail!("Host B expected 8 bytes, got {received_b}");
    }
    let ping = read_mem(&instance_b, &mut store_b, 200, 8)?;
    if ping != b"WasmPing" {
        bail!("Host B received wrong content: {:?}", String::from_utf8_lossy(&ping));
    }
    println!("Host B received: {}", String::from_utf8_lossy(&ping));

    // --- Host A receives "WasmPong!" ---
    poll_and_forward(&mut store_a);
    let received_a = {
        let f = instance_a
            .get_typed_func::<(i32, i32, i32, i32), i32>(&mut store_a, "tick_network")?;
        f.call(&mut store_a, (0, 6, 100, 8))?
    };
    println!("Host A received message of length {received_a}");
    if received_a != 9 {
        bail!("Host A expected 9 bytes, got {received_a}");
    }
    let pong = read_mem(&instance_a, &mut store_a, 200, 9)?;
    if pong != b"WasmPong!" {
        bail!("Host A received wrong content: {:?}", String::from_utf8_lossy(&pong));
    }
    println!("Host A received: {}", String::from_utf8_lossy(&pong));

    println!("✓ Wasm-to-Wasm P2P message sync successful");

    // --- Security test: invalid buffer pointer must return -1 safely ---
    store_a
        .data_mut()
        .incoming_messages_mut()
        .push_back(b"secret".to_vec());
    let bad_result = {
        let f = instance_a
            .get_typed_func::<(i32, i32), i32>(&mut store_a, "receive_at")?;
        f.call(&mut store_a, (999_999, 64))?
    };
    println!("Security test: receive at out-of-bounds pointer returned {bad_result}");
    if bad_result != -1 {
        bail!("security boundary FAILED: expected -1, got {bad_result}");
    }

    // A valid pointer still works after the failed attempt.
    store_a
        .data_mut()
        .incoming_messages_mut()
        .push_back(b"again".to_vec());
    let good_result = {
        let f = instance_a
            .get_typed_func::<(i32, i32), i32>(&mut store_a, "receive_at")?;
        f.call(&mut store_a, (300, 64))?
    };
    if good_result != 5 {
        bail!("expected 5 after valid receive, got {good_result}");
    }

    println!("✓ Memory safety boundary held on receive");
    Ok(())
}