use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tungstenite::{accept, Message, WebSocket};

struct PeerInfo {
    address: String,
    outbox: Sender<String>,
}

type PeerRegistry = Arc<Mutex<HashMap<String, PeerInfo>>>;

/// Runs the signaling server until the process exits. Blocks forever.
pub fn serve(bind_addr: &str) -> Result<()> {
    let listener = TcpListener::bind(bind_addr).context("failed to bind signaling server")?;
    println!("Signaling server listening on ws://{bind_addr}");

    let peers: PeerRegistry = Arc::new(Mutex::new(HashMap::new()));

    for stream in listener.incoming() {
        let stream = stream?;
        let peers = peers.clone();
        thread::spawn(move || {
            if let Err(e) = handle_peer(stream, peers) {
                eprintln!("peer connection error: {e:#}");
            }
        });
    }
    Ok(())
}

fn handle_peer(stream: TcpStream, peers: PeerRegistry) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    let mut ws = accept(stream).context("websocket handshake failed")?;

    let register = read_signal(&mut ws)?;
    if register["type"].as_str() != Some("register") {
        bail!("expected register message, got {register}");
    }
    let peer_id = register["peer_id"]
        .as_str()
        .context("missing peer_id")?
        .to_string();
    let address = register["address"]
        .as_str()
        .context("missing address")?
        .to_string();
    println!("Registered peer '{peer_id}' with UDP address {address}");

    let (outbox_tx, outbox_rx) = std::sync::mpsc::channel::<String>();
    peers
        .lock()
        .unwrap()
        .insert(peer_id.clone(), PeerInfo { address, outbox: outbox_tx });

    loop {
        if let Ok(msg) = outbox_rx.try_recv() {
            ws.send(Message::text(msg))?;
        }
        match ws.read() {
            Ok(Message::Text(t)) => {
                let v: Value = serde_json::from_str(t.as_str())?;
                handle_signal(&v, &peer_id, &peers)?;
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
    }

    peers.lock().unwrap().remove(&peer_id);
    println!("Peer '{peer_id}' disconnected");
    Ok(())
}

fn handle_signal(v: &Value, from_peer: &str, peers: &PeerRegistry) -> Result<()> {
    if let Some("request_connection") = v["type"].as_str() {
        let to_peer = v["to_peer"].as_str().context("missing to_peer")?.to_string();
        println!("Peer '{from_peer}' requests connection to '{to_peer}'");

        let (to_address, from_address, to_outbox, from_outbox) = {
            let registry = peers.lock().unwrap();
            match (registry.get(&to_peer), registry.get(from_peer)) {
                (Some(to_info), Some(from_info)) => (
                    to_info.address.clone(),
                    from_info.address.clone(),
                    to_info.outbox.clone(),
                    from_info.outbox.clone(),
                ),
                _ => {
                    let err = json!({"type":"error","message": format!("peer '{to_peer}' not found")})
                        .to_string();
                    if let Some(from_info) = registry.get(from_peer) {
                        let _ = from_info.outbox.send(err);
                    }
                    return Ok(());
                }
            }
        };

        to_outbox.send(
            json!({"type":"connection_info","peer_id": from_peer, "address": from_address})
                .to_string(),
        )?;
        from_outbox.send(
            json!({"type":"connection_info","peer_id": to_peer, "address": to_address})
                .to_string(),
        )?;
    }
    Ok(())
}

fn read_signal(ws: &mut WebSocket<TcpStream>) -> Result<Value> {
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => return Ok(serde_json::from_str(t.as_str())?),
            Ok(Message::Close(_)) => bail!("client closed connection"),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
    }
}
