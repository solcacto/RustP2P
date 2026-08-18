use anyhow::{bail, Result};
use host::host_state::HostState;
use host::peer_connection::PeerConnection;
use std::net::SocketAddr;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use wasmtime::{Engine, Store};

const SIGNAL_SERVER: &str = "127.0.0.1:9001";

fn peer_a() -> Result<()> {
    let engine = Engine::default();
    let mut store = Store::new(&engine, HostState::new("peer-a"));
    store.data_mut().set_peer_connection(Some(PeerConnection::new(
        "A",
        SIGNAL_SERVER,
        "127.0.0.1:0",
    )?));
    println!(
        "Peer A registered (UDP {})",
        store.data().peer_connection().unwrap().local_addr()?
    );

    println!("Peer A requesting connection to Peer B");
    let b_addr = store
        .data_mut()
        .peer_connection_mut()
        .unwrap()
        .request_connection("B")?;
    store.data_mut().connected_peers_mut().push("B".to_string());
    println!("Peer A discovered Peer B at {b_addr}");

    let pc = store.data().peer_connection().unwrap();
    pc.send_udp(b_addr, b"ping")?;
    println!("Peer A sent: ping");

    let mut buf = [0u8; 1024];
    let (len, from) = pc.recv_udp(&mut buf)?;
    let reply = &buf[..len];
    println!(
        "Peer A received: {} from {from}",
        String::from_utf8_lossy(reply)
    );
    if reply != b"pong" {
        bail!("expected 'pong', got {:?}", String::from_utf8_lossy(reply));
    }
    Ok(())
}

fn peer_b() -> Result<()> {
    let engine = Engine::default();
    let mut store = Store::new(&engine, HostState::new("peer-b"));
    store.data_mut().set_peer_connection(Some(PeerConnection::new(
        "B",
        SIGNAL_SERVER,
        "127.0.0.1:0",
    )?));
    println!(
        "Peer B registered (UDP {})",
        store.data().peer_connection().unwrap().local_addr()?
    );

    let a_addr: SocketAddr = store
        .data_mut()
        .peer_connection_mut()
        .unwrap()
        .wait_for_connection_info("A", Duration::from_secs(5))?;
    store.data_mut().connected_peers_mut().push("A".to_string());
    println!("Peer B learned Peer A at {a_addr}");

    let pc = store.data().peer_connection().unwrap();
    let mut buf = [0u8; 1024];
    let (len, from) = pc.recv_udp(&mut buf)?;
    let msg = &buf[..len];
    println!(
        "Peer B received: {} from {from}",
        String::from_utf8_lossy(msg)
    );
    if msg != b"ping" {
        bail!("expected 'ping', got {:?}", String::from_utf8_lossy(msg));
    }

    pc.send_udp(from, b"pong")?;
    println!("Peer B sent: pong");
    Ok(())
}

fn main() -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();

    let a = thread::spawn(move || {
        let r = peer_a();
        let _ = tx.send(("A", r));
    });
    let b = thread::spawn(move || {
        let r = peer_b();
        let _ = tx2.send(("B", r));
    });

    let mut results = Vec::new();
    for _ in 0..2 {
        results.push(rx.recv()?);
    }
    let _ = a.join();
    let _ = b.join();

    for (name, r) in &results {
        match r {
            Ok(()) => {}
            Err(e) => bail!("peer {name} failed: {e:#}"),
        }
    }

    println!("✓ P2P connection established");
    println!("✓ Message exchange successful");
    Ok(())
}
