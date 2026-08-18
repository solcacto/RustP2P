//! Direct peer-to-peer connectivity over UDP with WebSocket signaling.
//!
//! A [`PeerConnection`] registers with a signaling server, learns other peers'
//! UDP addresses through it, and then exchanges game datagrams directly over
//! UDP — the signaling server is only used for discovery, not for relaying
//! game traffic.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tungstenite::{client as ws_client, Message, WebSocket};

/// Wire tag marking a ping (17 bytes: tag + u64 timestamp ms + u64 nonce).
pub const PING_TAG: u8 = 0x0F;
/// Wire tag marking a pong (echoes the ping payload).
pub const PONG_TAG: u8 = 0x10;

/// Per-connection network statistics, updated as traffic flows.
#[derive(Debug, Clone, Default)]
pub struct PeerStats {
    /// Latest measured round-trip time (ms).
    pub rtt_ms: f64,
    /// UDP datagrams sent.
    pub packets_sent: u64,
    /// UDP datagrams received.
    pub packets_received: u64,
    /// Payload bytes sent.
    pub bytes_sent: u64,
    /// Payload bytes received.
    pub bytes_received: u64,
    /// Recent RTT samples (ms).
    pub rtt_samples: Vec<f64>,
}

impl PeerStats {
    /// Average RTT over the collected samples, if any.
    pub fn avg_rtt_ms(&self) -> Option<f64> {
        if self.rtt_samples.is_empty() {
            None
        } else {
            Some(self.rtt_samples.iter().sum::<f64>() / self.rtt_samples.len() as f64)
        }
    }
}

/// A P2P link to other game peers: one non-blocking UDP socket for game
/// traffic plus a WebSocket to the signaling server for address discovery.
pub struct PeerConnection {
    peer_id: String,
    signal_addr: String,
    udp: UdpSocket,
    ws: Option<WebSocket<TcpStream>>,
    peers: HashMap<String, SocketAddr>,
    stats: Mutex<PeerStats>,
}

impl PeerConnection {
    /// Opens the UDP socket, connects to the signaling server at `signal_addr`
    /// (for example `"127.0.0.1:9001"`), and registers `peer_id` with it.
    ///
    /// `udp_bind` is the local bind address for the game socket, e.g.
    /// `"127.0.0.1:0"` to let the OS pick a free port.
    pub fn new(peer_id: impl Into<String>, signal_addr: &str, udp_bind: &str) -> Result<Self> {
        let peer_id = peer_id.into();
        let udp = UdpSocket::bind(udp_bind)?;
        udp.set_nonblocking(true)?;

        let mut pc = Self {
            peer_id,
            signal_addr: signal_addr.to_string(),
            udp,
            ws: Some(Self::open_ws(signal_addr)?),
            peers: HashMap::new(),
            stats: Mutex::new(PeerStats::default()),
        };
        pc.register()?;
        Ok(pc)
    }

    /// A UDP-only peer connection (no signaling WebSocket) — used by tests and
    /// for direct links resolved from the chunk DHT.
    pub fn udp_only(peer_id: impl Into<String>, udp_bind: &str) -> Result<Self> {
        let udp = UdpSocket::bind(udp_bind)?;
        udp.set_nonblocking(true)?;
        Ok(Self {
            peer_id: peer_id.into(),
            signal_addr: String::new(),
            udp,
            ws: None,
            peers: HashMap::new(),
            stats: Mutex::new(PeerStats::default()),
        })
    }

    /// Establishes a fresh WebSocket connection to the signaling server.
    fn open_ws(signal_addr: &str) -> Result<WebSocket<TcpStream>> {
        let tcp = TcpStream::connect(signal_addr).with_context(|| {
            format!("signaling server at {signal_addr} not reachable (is it running?)")
        })?;
        tcp.set_read_timeout(Some(Duration::from_millis(100)))?;
        let url = format!("ws://{signal_addr}/");
        let (ws, _) = ws_client(url, tcp).context("signaling handshake failed")?;
        Ok(ws)
    }

    /// Sends this peer's `register` message to the signaling server.
    fn register(&mut self) -> Result<()> {
        let local_addr = self.udp.local_addr()?;
        self.send_signal(&json!({
            "type": "register",
            "peer_id": self.peer_id,
            "address": local_addr.to_string(),
        }))
    }

    /// Re-establishes the signaling WebSocket after the link went down, keeping
    /// the UDP socket (and therefore our P2P address and peer map) intact.
    pub fn reconnect(&mut self) -> Result<()> {
        self.ws = Some(Self::open_ws(&self.signal_addr)?);
        self.register()
    }

    /// Probes the signaling link for liveness.
    ///
    /// Reads one pending message: a clean `Close` or any I/O error means the
    /// link is dead; a timeout (nothing pending) means it is healthy. Stale
    /// messages the server had queued are discarded — the link is what matters.
    pub fn probe_signaling(&mut self) -> Result<()> {
        let Some(ws) = self.ws.as_mut() else { return Ok(()); };
        match ws.read() {
            Ok(Message::Close(_)) => bail!("signaling server closed the connection"),
            Ok(_) => Ok(()),
            Err(tungstenite::Error::Io(e))
                if e.kind() == io::ErrorKind::TimedOut
                    || e.kind() == io::ErrorKind::WouldBlock =>
            {
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Gracefully closes the signaling WebSocket. Dropping this connection
    /// afterwards closes the underlying TCP socket and UDP socket.
    pub fn shutdown(&mut self) -> Result<()> {
        if let Some(ws) = self.ws.as_mut() {
            ws.close(None)?;
            ws.flush()?;
        }
        Ok(())
    }

    /// Returns this node's registered peer id.
    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    /// Returns the local UDP socket address game traffic is sent from.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.udp.local_addr()?)
    }

    /// Returns the known UDP address of `peer_id`, if a link was established.
    pub fn peer_addr(&self, peer_id: &str) -> Option<&SocketAddr> {
        self.peers.get(peer_id)
    }

    /// Establishes a direct link to `peer_id` at `addr` — used when the chunk
    /// DHT resolves a zone's owner to its address without going through the
    /// signaling server.
    pub fn connect_direct(&mut self, peer_id: &str, addr: SocketAddr) {
        self.peers.insert(peer_id.to_string(), addr);
    }

    /// Asks the signaling server to connect us to `target` and waits for the
    /// target's address, which the server relays to us.
    pub fn request_connection(&mut self, target: &str) -> Result<SocketAddr> {
        self.send_signal(&json!({
            "type": "request_connection",
            "from_peer": self.peer_id,
            "to_peer": target,
        }))?;
        let addr = self.wait_for_connection_info(target, Duration::from_secs(5))?;
        Ok(addr)
    }

    /// Waits until the signaling server relays the address of `target`.
    pub fn wait_for_connection_info(&mut self, target: &str, timeout: Duration) -> Result<SocketAddr> {
        let deadline = Instant::now() + timeout;
        loop {
            let v = self.read_signal()?;
            match v["type"].as_str() {
                Some("connection_info") if v["peer_id"].as_str() == Some(target) => {
                    let addr = v["address"]
                        .as_str()
                        .ok_or_else(|| anyhow!("missing address"))?
                        .parse()?;
                    self.peers.insert(target.to_string(), addr);
                    return Ok(addr);
                }
                Some("error") => bail!("signaling error: {}", v["message"]),
                _ => {}
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for connection info from '{target}'");
            }
        }
    }

    /// Sends `data` as a single UDP datagram to `addr`, returning the number of
    /// bytes sent.
    pub fn send_udp(&self, addr: SocketAddr, data: &[u8]) -> Result<usize> {
        let n = self.udp.send_to(data, addr)?;
        let mut stats = self.stats.lock().unwrap();
        stats.packets_sent += 1;
        stats.bytes_sent += n as u64;
        Ok(n)
    }

    /// Pings every known peer and records the round-trip time when the pong
    /// arrives (measured during the next [`Self::poll_incoming_from`]).
    pub fn ping_all(&self) {
        for addr in self.peers.values() {
            let _ = self.send_ping(*addr);
        }
    }

    /// Tally received bytes/packets into the stats.
    fn count_received(&self, len: usize, _data: &[u8]) {
        let mut stats = self.stats.lock().unwrap();
        stats.packets_received += 1;
        stats.bytes_received += len as u64;
    }

    /// Records an RTT sample (ms).
    fn record_rtt(&self, ms: f64) {
        let mut stats = self.stats.lock().unwrap();
        stats.rtt_ms = ms;
        stats.rtt_samples.push(ms);
        if stats.rtt_samples.len() > 1000 {
            stats.rtt_samples.remove(0);
        }
    }

    /// Sends a ping (tag + timestamp + nonce) to `addr`.
    pub fn send_ping(&self, addr: SocketAddr) -> Result<()> {
        let mut buf = [0u8; 17];
        buf[0] = PING_TAG;
        let now = crate::chunk::now_ms();
        buf[1..9].copy_from_slice(&now.to_le_bytes());
        let nonce: u64 = now ^ (addr.port() as u64).wrapping_mul(0x9E37_79B9);
        buf[9..17].copy_from_slice(&nonce.to_le_bytes());
        self.send_udp(addr, &buf)?;
        Ok(())
    }

    /// Per-connection network statistics.
    pub fn stats(&self) -> PeerStats {
        self.stats.lock().unwrap().clone()
    }

    /// Blocks (up to five seconds) until a UDP datagram arrives, returning its
    /// payload and sender address.
    pub fn recv_udp(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.udp.recv_from(buf) {
                Ok(r) => {
                    self.count_received(r.0, &buf[..r.0]);
                    return Ok(r);
                }
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    if Instant::now() >= deadline {
                        bail!("timed out waiting for UDP message");
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Non-blocking receive; returns None if no datagram is pending.
    pub fn try_recv_udp(&self, buf: &mut [u8]) -> Option<(usize, SocketAddr)> {
        match self.udp.recv_from(buf) {
            Ok(r) => {
                self.count_received(r.0, &buf[..r.0]);
                Some(r)
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut =>
            {
                None
            }
            Err(_) => None,
        }
    }

    /// Drains all currently pending UDP datagrams from the socket.
    pub fn poll_incoming(&self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = [0u8; 65536];
        while let Ok((len, _)) = self.udp.recv_from(&mut buf) {
            out.push(buf[..len].to_vec());
            self.count_received(len, &buf[..len]);
        }
        out
    }

    /// Drains all pending UDP datagrams, preserving each sender's address.
    /// Ping/pong frames are answered and measured here, so game traffic only
    /// sees real payloads.
    pub fn poll_incoming_from(&self) -> Vec<(SocketAddr, Vec<u8>)> {
        let mut out = Vec::new();
        let mut buf = [0u8; 65536];
        while let Ok((len, addr)) = self.udp.recv_from(&mut buf) {
            self.count_received(len, &buf[..len]);
            let payload = &buf[..len];
            match payload.first() {
                Some(&PING_TAG) if len >= 17 => {
                    // Echo a pong carrying the same timestamp so the sender can
                    // measure the round trip.
                    let mut pong = [0u8; 17];
                    pong[0] = PONG_TAG;
                    pong[1..].copy_from_slice(&payload[1..17]);
                    let _ = self.udp.send_to(&pong, addr);
                }
                Some(&PONG_TAG) if len >= 17 => {
                    // Compute RTT from the echoed timestamp.
                    let sent_ms = u64::from_le_bytes(payload[1..9].try_into().unwrap());
                    let rtt = crate::chunk::now_ms().saturating_sub(sent_ms);
                    self.record_rtt(rtt as f64);
                }
                _ => out.push((addr, payload.to_vec())),
            }
        }
        out
    }

    /// Broadcasts `data` to every currently known peer.
    pub fn send_to_all(&self, data: &[u8]) -> Result<()> {
        for addr in self.peers.values() {
            self.udp.send_to(data, addr)?;
        }
        Ok(())
    }

    /// Reverse-looks up a known peer by its socket address.
    pub fn peer_id_for_addr(&self, addr: &SocketAddr) -> Option<&String> {
        self.peers.iter().find(|(_, a)| *a == addr).map(|(id, _)| id)
    }

    fn send_signal(&mut self, v: &Value) -> Result<()> {
        self.ws
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no signaling connection"))?
            .send(Message::text(v.to_string()))?;
        Ok(())
    }

    fn read_signal(&mut self) -> Result<Value> {
        let ws = self.ws.as_mut().ok_or_else(|| anyhow::anyhow!("no signaling connection"))?;
        loop {
            match ws.read() {
                Ok(Message::Text(t)) => return Ok(serde_json::from_str(t.as_str())?),
                Ok(Message::Close(_)) => bail!("signaling server closed connection"),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if e.kind() == io::ErrorKind::TimedOut
                        || e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}
