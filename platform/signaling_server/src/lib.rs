//! WebSocket signaling server for P2P peer discovery and WebRTC signaling.
//!
//! Peers connect over WebSocket, register a peer id plus their UDP address,
//! and request connections to each other. The server never relays game
//! traffic — it only exchanges `connection_info` messages so peers can talk
//! directly over UDP, and it **tricks** the WebRTC signaling messages
//! (`webrtc_offer`, `webrtc_answer`, `ice_candidate`) between peers so they can
//! negotiate an SDP/ICE session and punch through NAT. Disconnected peers are
//! removed from the registry so stale registrations never linger.
//!
//! A single peer may hold several WebSocket connections at once (one for UDP
//! address discovery, one per WebRTC signaling session); messages addressed to
//! a peer are fanned out to **all** of its connections.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tungstenite::{accept, Message, WebSocket};

#[derive(Clone)]
struct PeerInfo {
    /// Unique per-connection id so a peer's connections can be told apart.
    id: u64,
    address: String,
    /// Game CID this connection is lobbying for ("" = none / legacy).
    game: String,
    outbox: Sender<String>,
}

type PeerRegistry = Arc<Mutex<HashMap<String, Vec<PeerInfo>>>>;

/// Connection id counter, used to remove the correct socket on disconnect.
static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(1);

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
    let game = register["game"].as_str().unwrap_or("").to_string();

    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::SeqCst);
    let (outbox_tx, outbox_rx) = std::sync::mpsc::channel::<String>();
    peers
        .lock()
        .unwrap()
        .entry(peer_id.clone())
        .or_default()
        .push(PeerInfo {
            id: conn_id,
            address,
            game,
            outbox: outbox_tx,
        });
    println!(
        "Registered peer '{peer_id}' (connection #{conn_id}, {} UDP socket(s))",
        peers
            .lock()
            .unwrap()
            .get(&peer_id)
            .map(|l| l.len())
            .unwrap_or(0)
    );

    // The connection is removed on every exit path (clean close or error) so a
    // crashed/disconnected client never leaves a stale registration whose
    // outbox channel is closed.
    let result = serve_peer(&mut ws, &peer_id, conn_id, &peers, &outbox_rx);
    {
        let mut registry = peers.lock().unwrap();
        if let Some(list) = registry.get_mut(&peer_id) {
            list.retain(|info| info.id != conn_id);
            if list.is_empty() {
                registry.remove(&peer_id);
            }
        }
    }
    println!("Peer '{peer_id}' disconnected (connection #{conn_id})");
    result
}

fn serve_peer(
    ws: &mut WebSocket<TcpStream>,
    peer_id: &str,
    conn_id: u64,
    peers: &PeerRegistry,
    outbox_rx: &std::sync::mpsc::Receiver<String>,
) -> Result<()> {
    loop {
        if let Ok(msg) = outbox_rx.try_recv() {
            ws.send(Message::text(msg))?;
        }
        match ws.read() {
            Ok(Message::Text(t)) => {
                let v: Value = serde_json::from_str(t.as_str())?;
                handle_signal(&v, peer_id, conn_id, peers)?;
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock => {
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn handle_signal(v: &Value, from_peer: &str, conn_id: u64, peers: &PeerRegistry) -> Result<()> {
    match v["type"].as_str() {
        Some("request_connection") => {
            let to_peer = v["to_peer"]
                .as_str()
                .context("missing to_peer")?
                .to_string();
            println!("Peer '{from_peer}' requests connection to '{to_peer}'");

            let (to_list, from_list) = {
                let registry = peers.lock().unwrap();
                (
                    registry.get(&to_peer).cloned(),
                    registry.get(from_peer).cloned(),
                )
            };

            // If the target is unknown, report the error back to the requester.
            if to_list.is_none() {
                let err = json!({"type":"error","message": format!("peer '{to_peer}' not found")})
                    .to_string();
                if let Some(from_list) = &from_list {
                    for info in from_list {
                        let _ = info.outbox.send(err.clone());
                    }
                }
                return Ok(());
            }

            // Relay each peer's address to every socket the other peer holds.
            let (to_info, from_info) = {
                let registry = peers.lock().unwrap();
                let to_addr = registry
                    .get(&to_peer)
                    .and_then(|l| l.first())
                    .map(|i| i.address.clone());
                let from_addr = registry
                    .get(from_peer)
                    .and_then(|l| l.first())
                    .map(|i| i.address.clone());
                (to_addr, from_addr)
            };
            let (Some(to_address), Some(from_address)) = (to_info, from_info) else {
                return Ok(());
            };
            for info in &to_list.unwrap() {
                info.outbox.send(
                    json!({"type":"connection_info","peer_id": from_peer, "address": from_address})
                        .to_string(),
                )?;
            }
            if let Some(from_list) = from_list {
                for info in from_list {
                    info.outbox.send(
                        json!({"type":"connection_info","peer_id": to_peer, "address": to_address})
                            .to_string(),
                    )?;
                }
            }
        }
        Some("list_peers") => {
            // Return all currently registered peers (optionally filtered by
            // game) to the requester only, so a lobby can find a host.
            let filter = v["game"].as_str().unwrap_or("");
            let list: Vec<Value> = {
                let registry = peers.lock().unwrap();
                let mut out = Vec::new();
                for (pid, infos) in registry.iter() {
                    for info in infos {
                        if filter.is_empty() || info.game == filter {
                            out.push(json!({
                                "peer_id": pid,
                                "address": info.address,
                                "game": info.game,
                            }));
                        }
                    }
                }
                out
            };
            let reply = json!({"type":"peer_list","peers": list}).to_string();
            let registry = peers.lock().unwrap();
            if let Some(from_list) = registry.get(from_peer) {
                for info in from_list {
                    let _ = info.outbox.send(reply.clone());
                }
            }
        }
        // WebRTC SDP offers/answers and ICE candidates are relayed verbatim to
        // every socket of the target peer (the sender's own socket is skipped
        // so a multi-socket peer never echoes its own signaling).
        Some("webrtc_offer") | Some("webrtc_answer") | Some("ice_candidate") => {
            let to_peer = v["to_peer"]
                .as_str()
                .context("missing to_peer")?
                .to_string();
            let payload = v.to_string();
            let registry = peers.lock().unwrap();
            if let Some(list) = registry.get(&to_peer) {
                for info in list {
                    if info.id != conn_id {
                        info.outbox.send(payload.clone())?;
                    }
                }
            }
        }
        _ => {}
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
                if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::WouldBlock => {
            }
            Err(e) => return Err(e.into()),
        }
    }
}
