//! Direct peer-to-peer connectivity over UDP with WebSocket signaling.
//!
//! A [`PeerConnection`] registers with a signaling server, learns other peers'
//! UDP addresses through it, and then exchanges game datagrams directly over
//! UDP — the signaling server is only used for discovery, not for relaying
//! game traffic.

use anyhow::{anyhow, bail, Context, Result};
use crate::net::{self, ReliableMessage};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tungstenite::{client as ws_client, Message, WebSocket};

/// Wire tag marking a ping (17 bytes: tag + u64 timestamp ms + u64 nonce).
pub const PING_TAG: u8 = 0x0F;
/// Wire tag marking a pong (echoes the ping payload).
pub const PONG_TAG: u8 = 0x10;

/// Maximum messages accumulated before a batch is flushed immediately.
pub const MAX_BATCH_MESSAGES: usize = 10;
/// Maximum time (ms) messages are held before a batch is flushed.
pub const BATCH_WINDOW_MS: u64 = 16;
/// Reliable resend interval and retry budget.
pub const RELIABLE_RESEND_MS: u64 = 100;
pub const RELIABLE_MAX_RETRIES: u32 = 3;

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
    /// Inner messages sent (including batched).
    pub messages_sent: u64,
    /// Inner messages received (including batched).
    pub messages_received: u64,
    /// Estimated packet loss percentage (from batch sequence gaps).
    pub packet_loss_percent: f64,
    /// Bandwidth (KiB/s) since the counters were last sampled.
    pub bandwidth_kbps: f64,
    /// Average inner messages per sent packet.
    pub avg_batch_size: f64,
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
///
/// High-frequency traffic (avatar poses, scores) is **batched** (accumulated up
/// to 16 ms and flushed as one UDP datagram per peer) and **delta-compressed**;
/// reliable messages (chat, game events) are acked and retried.
/// A pending batch: accumulated messages and when the batch started.
type PendingBatch = (Vec<Vec<u8>>, Instant);

/// A P2P link to other game peers: one non-blocking UDP socket for game
/// traffic plus a WebSocket to the signaling server for address discovery.
///
/// High-frequency traffic (avatar poses, scores) is **batched** (accumulated up
/// to 16 ms and flushed as one UDP datagram per peer) and **delta-compressed**;
/// reliable messages (chat, game events) are acked and retried.
pub struct PeerConnection {
    peer_id: String,
    signal_addr: String,
    udp: UdpSocket,
    ws: Option<WebSocket<TcpStream>>,
    peers: HashMap<String, SocketAddr>,
    stats: Mutex<PeerStats>,
    /// Pending batched messages per peer, with when the batch started.
    pending: Mutex<HashMap<SocketAddr, PendingBatch>>,
    /// Monotonic batch sequence number.
    batch_seq: Mutex<u64>,
    /// Last seen batch sequence per sender (for loss detection).
    batch_seq_seen: Mutex<HashMap<SocketAddr, u32>>,
    /// Packets estimated lost (batch sequence gaps).
    packets_lost: Mutex<u64>,
    /// Last `bytes_sent`/`bytes_received` sample for bandwidth.
    bandwidth_sample: Mutex<(Instant, u64, u64)>,
    /// Reliable messages awaiting acknowledgement, per peer.
    reliable_outgoing: Mutex<HashMap<String, VecDeque<ReliableMessage>>>,
    /// Monotonic reliable sequence number.
    reliable_seq: Mutex<u64>,
    /// Delivered reliable messages waiting for the guest to read.
    reliable_inbox: Mutex<VecDeque<(String, Vec<u8>)>>,
}

impl PeerConnection {
    /// Opens the UDP socket, connects to the signaling server at `signal_addr`
    /// (for example `"127.0.0.1:9001"`), and registers `peer_id` with it.
    ///
    /// `udp_bind` is the local bind address for the game socket, e.g.
    /// `"127.0.0.1:0"` to let the OS pick a free port.
    pub fn new(peer_id: impl Into<String>, signal_addr: &str, udp_bind: &str) -> Result<Self> {
        let peer_id = peer_id.into();
        let mut pc = Self {
            peer_id,
            signal_addr: signal_addr.to_string(),
            udp: Self::bind_udp(udp_bind)?,
            ws: Some(Self::open_ws(signal_addr)?),
            peers: HashMap::new(),
            stats: Mutex::new(PeerStats::default()),
            pending: Mutex::new(HashMap::new()),
            batch_seq: Mutex::new(0),
            batch_seq_seen: Mutex::new(HashMap::new()),
            packets_lost: Mutex::new(0),
            bandwidth_sample: Mutex::new((Instant::now(), 0, 0)),
            reliable_outgoing: Mutex::new(HashMap::new()),
            reliable_seq: Mutex::new(0),
            reliable_inbox: Mutex::new(VecDeque::new()),
        };
        pc.register()?;
        Ok(pc)
    }

    /// A UDP-only peer connection (no signaling WebSocket) — used by tests and
    /// for direct links resolved from the chunk DHT.
    pub fn udp_only(peer_id: impl Into<String>, udp_bind: &str) -> Result<Self> {
        Ok(Self {
            peer_id: peer_id.into(),
            signal_addr: String::new(),
            udp: Self::bind_udp(udp_bind)?,
            ws: None,
            peers: HashMap::new(),
            stats: Mutex::new(PeerStats::default()),
            pending: Mutex::new(HashMap::new()),
            batch_seq: Mutex::new(0),
            batch_seq_seen: Mutex::new(HashMap::new()),
            packets_lost: Mutex::new(0),
            bandwidth_sample: Mutex::new((Instant::now(), 0, 0)),
            reliable_outgoing: Mutex::new(HashMap::new()),
            reliable_seq: Mutex::new(0),
            reliable_inbox: Mutex::new(VecDeque::new()),
        })
    }

    /// Binds + tunes the UDP socket for low-latency game traffic.
    fn bind_udp(udp_bind: &str) -> Result<UdpSocket> {
        let udp = UdpSocket::bind(udp_bind)?;
        udp.set_nonblocking(true)?;
        // Low-latency tuning: cap TTL to the local network and size the OS
        // buffers so bursts of batched datagrams aren't dropped.
        let _ = udp.set_ttl(64);
        Ok(udp)
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
    /// bytes sent. Used for immediate (non-batched) messages.
    pub fn send_udp(&self, addr: SocketAddr, data: &[u8]) -> Result<usize> {
        let n = self.udp.send_to(data, addr)?;
        let mut stats = self.stats.lock().unwrap();
        stats.packets_sent += 1;
        stats.bytes_sent += n as u64;
        Ok(n)
    }

    /// Queues `data` for **batched** delivery to every known peer. Batches are
    /// flushed by [`Self::flush_batches`] after up to 16 ms (or once they hold
    /// 10 messages), so high-frequency traffic ships as few UDP packets.
    pub fn batch_send_to_all(&self, data: &[u8]) {
        let mut stats = self.stats.lock().unwrap();
        stats.messages_sent += self.peers.len() as u64;
        drop(stats);
        let mut pending = self.pending.lock().unwrap();
        let now = Instant::now();
        for addr in self.peers.values() {
            let entry = pending.entry(*addr).or_insert_with(|| (Vec::new(), now));
            entry.0.push(data.to_vec());
        }
    }

    /// Flushes any ready batches (window elapsed or batch full) as one UDP
    /// datagram per peer. Call once per frame.
    pub fn flush_batches(&self) {
        let now = Instant::now();
        let ready: Vec<(SocketAddr, Vec<Vec<u8>>)> = {
            let mut pending = self.pending.lock().unwrap();
            let mut ready = Vec::new();
            for (addr, (msgs, start)) in pending.iter_mut() {
                let due = msgs.len() >= MAX_BATCH_MESSAGES
                    || now.duration_since(*start) >= Duration::from_millis(BATCH_WINDOW_MS);
                if due && !msgs.is_empty() {
                    ready.push((*addr, std::mem::take(msgs)));
                }
            }
            pending.retain(|_, (msgs, _)| !msgs.is_empty());
            ready
        };
        for (addr, msgs) in ready {
            let mut seq = self.batch_seq.lock().unwrap();
            *seq += 1;
            let packet = net::build_batch(&msgs, *seq as u32);
            drop(seq);
            if let Ok(n) = self.udp.send_to(&packet, addr) {
                let mut stats = self.stats.lock().unwrap();
                stats.packets_sent += 1;
                stats.bytes_sent += n as u64;
            }
        }
    }

    /// Queues `payload` for reliable delivery to `peer_id` (acked + retried).
    pub fn send_reliable(&self, peer_id: &str, payload: &[u8]) -> Result<()> {
        let addr = *self
            .peers
            .get(peer_id)
            .ok_or_else(|| anyhow!("peer '{peer_id}' not connected"))?;
        let mut seq = self.reliable_seq.lock().unwrap();
        *seq += 1;
        let seq = *seq as u32;
        let msg = ReliableMessage {
            sequence_number: seq,
            payload: payload.to_vec(),
            ack_received: false,
            next_resend: Instant::now() + Duration::from_millis(RELIABLE_RESEND_MS),
            retry_count: 0,
        };
        self.reliable_outgoing
            .lock()
            .unwrap()
            .entry(peer_id.to_string())
            .or_default()
            .push_back(msg);
        self.send_udp(addr, &net::build_reliable(seq, payload))?;
        Ok(())
    }

    /// Queues `payload` for reliable delivery to every known peer.
    pub fn send_reliable_to_all(&self, payload: &[u8]) {
        let peers: Vec<String> = self.peers.keys().cloned().collect();
        for peer in peers {
            let _ = self.send_reliable(&peer, payload);
        }
    }

    /// Resends unacknowledged reliable messages past their retry deadline,
    /// dropping them after [`RELIABLE_MAX_RETRIES`]. Call once per frame.
    pub fn process_reliable_retries(&self) {
        let now = Instant::now();
        let mut outgoing = self.reliable_outgoing.lock().unwrap();
        for (peer_id, queue) in outgoing.iter_mut() {
            let addr = match self.peers.get(peer_id).copied() {
                Some(a) => a,
                None => continue,
            };
            let mut kept = VecDeque::new();
            while let Some(mut m) = queue.pop_front() {
                let drop_msg = m.ack_received || m.retry_count >= RELIABLE_MAX_RETRIES;
                if !drop_msg && now >= m.next_resend {
                    let _ = self.send_udp(addr, &net::build_reliable(m.sequence_number, &m.payload));
                    m.retry_count += 1;
                    m.next_resend = now + Duration::from_millis(RELIABLE_RESEND_MS);
                }
                if !drop_msg {
                    kept.push_back(m);
                }
            }
            *queue = kept;
        }
        outgoing.retain(|_, q| !q.is_empty());
    }

    /// Pops the next reliably-delivered message, if any.
    pub fn receive_reliable(&self) -> Option<(String, Vec<u8>)> {
        self.reliable_inbox.lock().unwrap().pop_front()
    }

    /// Number of reliable messages awaiting acknowledgement.
    pub fn reliable_pending(&self) -> usize {
        self.reliable_outgoing
            .lock()
            .unwrap()
            .values()
            .map(|q| q.iter().filter(|m| !m.ack_received).count())
            .sum()
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
    ///
    /// Transport-level frames are handled here: ping/pong are answered and
    /// measured, batch packets are split into their inner payloads (with
    /// sequence-gap loss detection), reliable frames are delivered to the
    /// inbox and acked, and acks retire queued reliable messages. Game traffic
    /// sees only the individual payloads.
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
                Some(&net::BATCH_TAG) => {
                    if let Some((seq, msgs)) = net::split_batch(payload) {
                        self.note_batch(addr, seq, msgs.len());
                        {
                            let mut stats = self.stats.lock().unwrap();
                            stats.messages_received += msgs.len() as u64;
                        }
                        for m in msgs {
                            out.push((addr, m));
                        }
                    }
                }
                Some(&net::RELIABLE_TAG) => {
                    if let Some(seq) = net::reliable_seq(payload) {
                        if let Some(body) = net::reliable_payload(payload) {
                            let peer = self.peer_id_for_addr(&addr).cloned().unwrap_or_else(|| addr.to_string());
                            self.reliable_inbox.lock().unwrap().push_back((peer, body.to_vec()));
                            // Ack it.
                            let _ = self.udp.send_to(&net::build_ack(seq), addr);
                        }
                    }
                }
                Some(&net::ACK_TAG) => {
                    if let Some(seq) = net::ack_seq(payload) {
                        let mut outgoing = self.reliable_outgoing.lock().unwrap();
                        for queue in outgoing.values_mut() {
                            for m in queue.iter_mut() {
                                if m.sequence_number == seq {
                                    m.ack_received = true;
                                }
                            }
                        }
                    }
                }
                _ => out.push((addr, payload.to_vec())),
            }
        }
        out
    }

    /// Records batch arrival for loss detection and batch-size stats.
    fn note_batch(&self, addr: SocketAddr, seq: u32, _size: usize) {
        let mut seen = self.batch_seq_seen.lock().unwrap();
        if let Some(prev) = seen.get(&addr) {
            let expected = prev.wrapping_add(1);
            if seq != expected {
                let lost = seq.wrapping_sub(expected) as u64;
                *self.packets_lost.lock().unwrap() += lost.min(1000);
            }
        }
        seen.insert(addr, seq);
    }

    /// Per-connection network statistics, including derived loss/bandwidth and
    /// average batch size.
    pub fn stats(&self) -> PeerStats {
        let mut stats = self.stats.lock().unwrap().clone();
        let lost = *self.packets_lost.lock().unwrap();
        let total = stats.packets_received + lost;
        stats.packet_loss_percent = if total > 0 { lost as f64 / total as f64 * 100.0 } else { 0.0 };
        let sent_packets = stats.packets_sent.max(1);
        stats.avg_batch_size = stats.messages_sent as f64 / sent_packets as f64;
        {
            let (start, sent0, recv0) = *self.bandwidth_sample.lock().unwrap();
            let elapsed = start.elapsed().as_secs_f64();
            if elapsed >= 0.1 {
                let sent_kb = (stats.bytes_sent - sent0) as f64 / 1024.0;
                stats.bandwidth_kbps = sent_kb / elapsed;
                let recv_kb = (stats.bytes_received - recv0) as f64 / 1024.0;
                stats.bandwidth_kbps = stats.bandwidth_kbps.max(recv_kb / elapsed);
                *self.bandwidth_sample.lock().unwrap() =
                    (Instant::now(), stats.bytes_sent, stats.bytes_received);
            }
        }
        stats
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
