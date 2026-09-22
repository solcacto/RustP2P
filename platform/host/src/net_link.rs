//! A single network link abstraction over two transports:
//!
//! - [`NetLink::Udp`] wraps the classic [`PeerConnection`] (UDP discovery +
//!   direct datagrams against a local signaling server). Used for local
//!   development and same-LAN play.
//! - [`NetLink::Webrtc`] wraps a [`WebRtcSession`]: one WebRTC data channel per
//!   remote peer, relayed through the deployed Cloudflare Worker. Every remote
//!   peer is assigned a stable *synthetic* socket address (from the
//!   carrier-grade NAT `100.64.0.0/10` range) so the rest of the host —
//!   chunk DHT, zone handoff, guest-facing APIs — keeps its existing
//!   address-keyed model unchanged.
//!
//! Both transports speak the same wire framing (`net::`): batched messages,
//! reliable frames + acks, and ping/pong for RTT. The WebRTC data channel is
//! reliable + ordered by default, so the reliable overlay degrades to
//! framing-only (no resends needed).

use std::collections::{HashMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use webrtc::ice_transport::ice_server::RTCIceServer;

use crate::net;
use crate::peer_connection::{PeerConnection, RELIABLE_MAX_RETRIES, RELIABLE_RESEND_MS};
use crate::webrtc_connection::{WebRtcConnection, WebRtcStatus};

/// A reliable message in flight over the (lossy) WebRTC data channel, tracking
/// ack + retry state exactly like the UDP path's [`PeerConnection`].
struct ReliableMessage {
    sequence_number: u32,
    payload: Vec<u8>,
    ack_received: bool,
    next_resend: std::time::Instant,
    retry_count: u32,
}

/// Default handshake budget for each WebRTC link (SDP + ICE + DTLS + SCTP).
pub const WEBRTC_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// The current network transport, configured at startup.
pub enum NetLink {
    /// Classic UDP discovery + direct datagrams (local signaling server).
    Udp(Box<PeerConnection>),
    /// WebRTC data channels relayed through the configured signaling server.
    Webrtc(Box<WebRtcSession>),
}

/// Derives a stable synthetic address for a peer id from the carrier-grade NAT
/// range (`100.64.0.0/10`). Deterministic per id, so a peer that reconnects
/// keeps the same address and the chunk DHT stays coherent.
fn synthetic_addr(peer_id: &str) -> SocketAddr {
    let mut h1: u32 = 0x811c_9dc5;
    let mut h2: u32 = 0x0100_0193;
    for b in peer_id.bytes() {
        h1 = (h1 ^ u32::from(b)).wrapping_mul(0x0100_0193);
        h2 = h2.wrapping_mul(0x9E37_79B9) ^ u32::from(b);
    }
    SocketAddr::new(
        Ipv4Addr::new(100, 64, (h1 >> 8) as u8, h1 as u8).into(),
        9000 + (h2 & 0x03ff) as u16,
    )
}

/// A WebRTC-backed peer session: one data channel per remote peer, all relayed
/// through the same signaling server (e.g. the deployed Cloudflare Worker).
pub struct WebRtcSession {
    local_id: String,
    signal_addr: String,
    ice_servers: Vec<RTCIceServer>,
    conns: HashMap<String, WebRtcConnection>,
    addrs: HashMap<String, SocketAddr>,
    reliable_inbox: Mutex<VecDeque<(String, Vec<u8>)>>,
    reliable_outgoing: Mutex<HashMap<String, VecDeque<ReliableMessage>>>,
    reliable_seq: Mutex<u64>,
}

impl WebRtcSession {
    /// Starts an empty session. Peers are attached lazily via
    /// [`Self::request_connection`].
    pub fn new(
        local_id: impl Into<String>,
        signal_addr: impl Into<String>,
        ice_servers: Vec<RTCIceServer>,
    ) -> Self {
        Self {
            local_id: local_id.into(),
            signal_addr: signal_addr.into(),
            ice_servers,
            conns: HashMap::new(),
            addrs: HashMap::new(),
            reliable_inbox: Mutex::new(VecDeque::new()),
            reliable_outgoing: Mutex::new(HashMap::new()),
            reliable_seq: Mutex::new(0),
        }
    }

    fn addr_for(&self, peer_id: &str) -> SocketAddr {
        self.addrs
            .get(peer_id)
            .copied()
            .unwrap_or_else(|| synthetic_addr(peer_id))
    }

    fn conn_for(&self, peer_id: &str) -> Option<&WebRtcConnection> {
        self.conns.get(peer_id)
    }

    /// Connects to `remote_id` over WebRTC, blocking until the data channel
    /// opens (or [`WEBRTC_HANDSHAKE_TIMEOUT`] elapses). Returns the synthetic
    /// address used by the rest of the host for this peer.
    pub fn request_connection(&mut self, remote_id: &str) -> Result<SocketAddr> {
        if self.conns.contains_key(remote_id) {
            return Ok(self.addr_for(remote_id));
        }
        let conn = WebRtcConnection::connect_with_config(
            self.local_id.clone(),
            remote_id,
            &self.signal_addr,
            WEBRTC_HANDSHAKE_TIMEOUT,
            Some(self.ice_servers.clone()),
        )
        .context(format!("WebRTC link to '{remote_id}' failed"))?;
        let addr = synthetic_addr(remote_id);
        self.addrs.insert(remote_id.to_string(), addr);
        self.conns.insert(remote_id.to_string(), conn);
        Ok(addr)
    }

    /// Connects to the peer hosting a chunk, resolving by id (the address is
    /// only used as a DHT key and maps to the same synthetic address).
    pub fn connect_direct(&mut self, peer_id: &str, _addr: SocketAddr) {
        if !self.conns.contains_key(peer_id) {
            let _ = self.request_connection(peer_id);
        }
    }

    /// Drains every data channel and dispatches the same wire tags as
    /// [`PeerConnection::poll_incoming_from`]: reliable frames land in the
    /// reliable inbox (and are acked), ack frames update the reliable queue,
    /// everything else is returned as an incoming datagram from the peer.
    pub fn poll_incoming_from(&self) -> Vec<(SocketAddr, Vec<u8>)> {
        let mut out = Vec::new();
        let peers: Vec<String> = self.conns.keys().cloned().collect();
        for peer_id in peers {
            let conn = match self.conns.get(&peer_id) {
                Some(c) => c,
                None => continue,
            };
            for payload in conn.poll_received() {
                match payload.first() {
                    Some(&net::RELIABLE_TAG) => {
                        if let Some(seq) = net::reliable_seq(&payload) {
                            if let Some(body) = net::reliable_payload(&payload) {
                                self.reliable_inbox
                                    .lock()
                                    .unwrap()
                                    .push_back((peer_id.clone(), body.to_vec()));
                                let _ = conn.send(&net::build_ack(seq));
                            }
                        }
                    }
                    Some(&net::ACK_TAG) => {
                        if let Some(seq) = net::ack_seq(&payload) {
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
                    _ => out.push((self.addr_for(&peer_id), payload)),
                }
            }
        }
        out
    }

    /// Sends `data` to the peer identified by `addr` (synthetic address).
    pub fn send_udp(&self, addr: SocketAddr, data: &[u8]) -> Result<usize> {
        let peer_id = self
            .addrs
            .iter()
            .find(|(_, a)| **a == addr)
            .map(|(id, _)| id.clone())
            .ok_or_else(|| anyhow!("peer at {addr} not connected"))?;
        let conn = self
            .conn_for(&peer_id)
            .ok_or_else(|| anyhow!("peer '{peer_id}' not connected"))?;
        conn.send(data)?;
        Ok(data.len())
    }

    /// Broadcasts `data` immediately to every peer.
    pub fn send_to_all(&self, data: &[u8]) -> Result<()> {
        for conn in self.conns.values() {
            conn.send(data)?;
        }
        Ok(())
    }

    /// Queues `data` for batched delivery to every peer.
    pub fn batch_send_to_all(&self, data: &[u8]) {
        for conn in self.conns.values() {
            conn.batch_send(data);
        }
    }

    /// Flushes pending batches on every link. Call once per frame.
    pub fn flush_batches(&self) {
        for conn in self.conns.values() {
            conn.flush_batches();
        }
    }

    /// Queues `payload` for reliable delivery (acked + retried over the lossy
    /// data channel, mirroring the UDP path's overlay).
    pub fn send_reliable(&self, peer_id: &str, payload: &[u8]) -> Result<()> {
        let conn = self
            .conn_for(peer_id)
            .ok_or_else(|| anyhow!("peer '{peer_id}' not connected"))?;
        let mut seq = self.reliable_seq.lock().unwrap();
        *seq += 1;
        let seq = *seq as u32;
        let msg = ReliableMessage {
            sequence_number: seq,
            payload: payload.to_vec(),
            ack_received: false,
            next_resend: std::time::Instant::now() + Duration::from_millis(RELIABLE_RESEND_MS),
            retry_count: 0,
        };
        self.reliable_outgoing
            .lock()
            .unwrap()
            .entry(peer_id.to_string())
            .or_default()
            .push_back(msg);
        conn.send(&net::build_reliable(seq, payload))?;
        Ok(())
    }

    /// Resends unacknowledged reliable messages past their retry deadline,
    /// dropping them after [`RELIABLE_MAX_RETRIES`]. Call once per frame.
    pub fn process_reliable_retries(&self) {
        let now = std::time::Instant::now();
        let mut outgoing = self.reliable_outgoing.lock().unwrap();
        for (peer_id, queue) in outgoing.iter_mut() {
            let conn = match self.conns.get(peer_id) {
                Some(c) => c,
                None => continue,
            };
            let mut kept = VecDeque::new();
            while let Some(mut m) = queue.pop_front() {
                let drop_msg = m.ack_received || m.retry_count >= RELIABLE_MAX_RETRIES;
                if !drop_msg && now >= m.next_resend {
                    let _ = conn.send(&net::build_reliable(m.sequence_number, &m.payload));
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

    /// Pings every peer; pongs are measured on the next poll.
    pub fn ping_all(&self) {
        for conn in self.conns.values() {
            conn.ping();
        }
    }

    /// Checks the liveness of every link's signaling session.
    pub fn probe_signaling(&self) -> Result<()> {
        for (peer_id, conn) in &self.conns {
            if let WebRtcStatus::Failed(e) = conn.status() {
                return Err(anyhow!("WebRTC link to '{peer_id}' failed: {e}"));
            }
        }
        Ok(())
    }

    /// Re-establishes any failed links (the data channel, unlike UDP, carries
    /// its own signaling lifecycle; a WS drop surfaces as `Failed` here).
    pub fn reconnect(&mut self) -> Result<()> {
        let failed: Vec<String> = self
            .conns
            .iter()
            .filter(|(_, c)| matches!(c.status(), WebRtcStatus::Failed(_)))
            .map(|(id, _)| id.clone())
            .collect();
        let mut first_err: Option<anyhow::Error> = None;
        for peer_id in failed {
            self.conns.remove(&peer_id);
            if let Err(e) = self.request_connection(&peer_id) {
                first_err.get_or_insert(e);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Average RTT across every open link (for the HUD / perf report).
    pub fn avg_rtt_ms(&self) -> Option<f64> {
        let mut samples = Vec::new();
        for conn in self.conns.values() {
            if let Some(avg) = conn.avg_rtt_ms() {
                samples.push(avg);
            }
        }
        if samples.is_empty() {
            None
        } else {
            Some(samples.iter().sum::<f64>() / samples.len() as f64)
        }
    }

    /// Closes every data channel and stops its runtime thread.
    pub fn shutdown(&mut self) {
        for (peer_id, conn) in self.conns.drain() {
            if let Err(e) = conn.close() {
                eprintln!("[net] closing WebRTC link to '{peer_id}' failed: {e}");
            }
        }
    }
}

impl NetLink {
    /// Creates a configured link: WebRTC when the signaling URL is a full
    /// `ws://`/`wss://` endpoint (deployed Worker), else the UDP path.
    pub fn new(
        local_id: impl Into<String>,
        signal_addr: &str,
        udp_bind: &str,
        ice_servers: Vec<RTCIceServer>,
    ) -> Result<Self> {
        let trimmed = signal_addr.trim_end_matches('/');
        if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
            Ok(NetLink::Webrtc(Box::new(WebRtcSession::new(
                local_id,
                signal_addr,
                ice_servers,
            ))))
        } else {
            Ok(NetLink::Udp(Box::new(PeerConnection::new(
                local_id,
                signal_addr,
                udp_bind,
            )?)))
        }
    }

    pub fn local_id(&self) -> &str {
        match self {
            NetLink::Udp(pc) => pc.peer_id(),
            NetLink::Webrtc(s) => &s.local_id,
        }
    }

    pub fn local_addr(&self) -> Result<SocketAddr> {
        match self {
            NetLink::Udp(pc) => pc.local_addr(),
            NetLink::Webrtc(s) => Ok(synthetic_addr(&s.local_id)),
        }
    }

    pub fn peer_addr(&self, peer_id: &str) -> Option<&SocketAddr> {
        match self {
            NetLink::Udp(pc) => pc.peer_addr(peer_id),
            NetLink::Webrtc(s) => s.addrs.get(peer_id),
        }
    }

    pub fn peer_id_for_addr(&self, addr: &SocketAddr) -> Option<&String> {
        match self {
            NetLink::Udp(pc) => pc.peer_id_for_addr(addr),
            NetLink::Webrtc(s) => s.addrs.iter().find(|(_, a)| *a == addr).map(|(id, _)| id),
        }
    }

    pub fn request_connection(&mut self, target: &str) -> Result<SocketAddr> {
        match self {
            NetLink::Udp(pc) => pc.request_connection(target),
            NetLink::Webrtc(s) => s.request_connection(target),
        }
    }

    /// Game-aware constructor for lobby mode (UDP path tags register with
    /// game CID; WebRTC path ignores game for now).
    pub fn new_with_game(
        local_id: impl Into<String>,
        signal_addr: &str,
        udp_bind: &str,
        ice_servers: Vec<RTCIceServer>,
        game: &str,
    ) -> Result<Self> {
        let trimmed = signal_addr.trim_end_matches('/');
        if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
            NetLink::new(local_id, signal_addr, udp_bind, ice_servers)
        } else {
            Ok(NetLink::Udp(Box::new(PeerConnection::new_with_game(
                local_id,
                signal_addr,
                udp_bind,
                game,
            )?)))
        }
    }

    /// Lobby query: currently registered peers for `game` (UDP only).
    pub fn list_peers(&mut self, game: &str) -> Result<Vec<(String, SocketAddr, String)>> {
        match self {
            NetLink::Udp(pc) => pc.list_peers(game),
            NetLink::Webrtc(_) => Ok(Vec::new()),
        }
    }

    /// Short-timeout connect for lobby join (UDP only; WebRTC uses full handshake).
    pub fn try_connect(&mut self, target: &str, timeout: Duration) -> Result<SocketAddr> {
        match self {
            NetLink::Udp(pc) => pc.try_connect(target, timeout),
            NetLink::Webrtc(s) => s.request_connection(target),
        }
    }

    /// Non-blocking drain of pending signaling arrivals (late joiners).
    pub fn poll_signaling(&mut self) -> usize {
        match self {
            NetLink::Udp(pc) => pc.poll_signaling(),
            NetLink::Webrtc(_) => 0,
        }
    }

    /// UDP-only diagnostic: blocks until the connection_info for `target`
    /// arrives. Not meaningful over a WebRTC link.
    pub fn wait_for_connection_info(
        &mut self,
        target: &str,
        timeout: Duration,
    ) -> Result<SocketAddr> {
        match self {
            NetLink::Udp(pc) => pc.wait_for_connection_info(target, timeout),
            NetLink::Webrtc(_) => Err(anyhow!(
                "wait_for_connection_info is only valid on the UDP transport"
            )),
        }
    }

    /// UDP-only diagnostic: blocks until a raw UDP datagram arrives. Not
    /// meaningful over a WebRTC link.
    pub fn recv_udp(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr)> {
        match self {
            NetLink::Udp(pc) => pc.recv_udp(buf),
            NetLink::Webrtc(_) => Err(anyhow!("recv_udp is only valid on the UDP transport")),
        }
    }

    pub fn connect_direct(&mut self, peer_id: &str, addr: SocketAddr) {
        match self {
            NetLink::Udp(pc) => pc.connect_direct(peer_id, addr),
            NetLink::Webrtc(s) => s.connect_direct(peer_id, addr),
        }
    }

    pub fn send_udp(&self, addr: SocketAddr, data: &[u8]) -> Result<usize> {
        match self {
            NetLink::Udp(pc) => pc.send_udp(addr, data),
            NetLink::Webrtc(s) => s.send_udp(addr, data),
        }
    }

    pub fn send_to_all(&self, data: &[u8]) -> Result<()> {
        match self {
            NetLink::Udp(pc) => pc.send_to_all(data),
            NetLink::Webrtc(s) => s.send_to_all(data),
        }
    }

    pub fn batch_send_to_all(&self, data: &[u8]) {
        match self {
            NetLink::Udp(pc) => pc.batch_send_to_all(data),
            NetLink::Webrtc(s) => s.batch_send_to_all(data),
        }
    }

    pub fn flush_batches(&self) {
        match self {
            NetLink::Udp(pc) => pc.flush_batches(),
            NetLink::Webrtc(s) => s.flush_batches(),
        }
    }

    pub fn send_reliable(&self, peer_id: &str, payload: &[u8]) -> Result<()> {
        match self {
            NetLink::Udp(pc) => pc.send_reliable(peer_id, payload),
            NetLink::Webrtc(s) => s.send_reliable(peer_id, payload),
        }
    }

    pub fn process_reliable_retries(&self) {
        match self {
            NetLink::Udp(pc) => pc.process_reliable_retries(),
            NetLink::Webrtc(s) => s.process_reliable_retries(),
        }
    }

    pub fn receive_reliable(&self) -> Option<(String, Vec<u8>)> {
        match self {
            NetLink::Udp(pc) => pc.receive_reliable(),
            NetLink::Webrtc(s) => s.receive_reliable(),
        }
    }

    pub fn reliable_pending(&self) -> usize {
        match self {
            NetLink::Udp(pc) => pc.reliable_pending(),
            NetLink::Webrtc(s) => s.reliable_pending(),
        }
    }

    pub fn ping_all(&self) {
        match self {
            NetLink::Udp(pc) => pc.ping_all(),
            NetLink::Webrtc(s) => s.ping_all(),
        }
    }

    pub fn probe_signaling(&mut self) -> Result<()> {
        match self {
            NetLink::Udp(pc) => pc.probe_signaling(),
            NetLink::Webrtc(s) => s.probe_signaling(),
        }
    }

    pub fn reconnect(&mut self) -> Result<()> {
        match self {
            NetLink::Udp(pc) => pc.reconnect(),
            NetLink::Webrtc(s) => s.reconnect(),
        }
    }

    /// Average RTT across the link (for the HUD / perf report).
    pub fn avg_rtt_ms(&self) -> Option<f64> {
        match self {
            NetLink::Udp(pc) => pc.stats().avg_rtt_ms(),
            NetLink::Webrtc(s) => s.avg_rtt_ms(),
        }
    }

    pub fn poll_incoming_from(&self) -> Vec<(SocketAddr, Vec<u8>)> {
        match self {
            NetLink::Udp(pc) => pc.poll_incoming_from(),
            NetLink::Webrtc(s) => s.poll_incoming_from(),
        }
    }

    /// Drains incoming messages, dropping sender addresses (raw framing).
    pub fn poll_incoming(&self) -> Vec<Vec<u8>> {
        self.poll_incoming_from()
            .into_iter()
            .map(|(_, m)| m)
            .collect()
    }

    pub fn shutdown(&mut self) {
        match self {
            NetLink::Udp(pc) => {
                let _ = pc.shutdown();
            }
            NetLink::Webrtc(s) => s.shutdown(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    /// Binds an ephemeral port, then starts the signaling server there. The
    /// ready port is returned.
    fn spawn_signaling() -> String {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        let addr = format!("127.0.0.1:{port}");
        let addr_clone = addr.clone();
        thread::spawn(move || {
            let _ = signaling_server::serve(&addr_clone);
        });
        for _ in 0..60 {
            if std::net::TcpStream::connect(&addr).is_ok() {
                return addr;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("signaling server at {addr} did not come up");
    }

    fn stun_server() -> RTCIceServer {
        RTCIceServer {
            urls: vec![crate::webrtc_connection::DEFAULT_STUN_URL.to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn synthetic_addrs_are_deterministic() {
        assert_eq!(synthetic_addr("PeerA"), synthetic_addr("PeerA"));
        assert_ne!(synthetic_addr("PeerA"), synthetic_addr("PeerB"));
        let addr = synthetic_addr("PeerA");
        assert_eq!(addr.port(), synthetic_addr("PeerA").port());
        let _ = addr; // used
    }

    #[test]
    fn udp_link_swaps_plain_and_reliable_messages() {
        let signal = spawn_signaling();
        let mut a = NetLink::Udp(Box::new(
            PeerConnection::new("A", &signal, "127.0.0.1:0").expect("register A"),
        ));
        let mut b = NetLink::Udp(Box::new(
            PeerConnection::new("B", &signal, "127.0.0.1:0").expect("register B"),
        ));
        let b_addr = a.request_connection("B").expect("A discovers B");
        let a_addr = b.request_connection("A").expect("B discovers A");

        a.send_udp(b_addr, b"hello").expect("plain send");
        a.send_reliable("B", b"important").expect("reliable send");
        a.batch_send_to_all(b"batched");
        thread::sleep(Duration::from_millis(20));
        a.flush_batches();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut got_plain = false;
        let mut got_batch = false;
        while !(got_plain && got_batch) {
            assert!(
                std::time::Instant::now() < deadline,
                "B never received plain/batched messages"
            );
            for (_, m) in b.poll_incoming_from() {
                got_plain |= m == b"hello";
                got_batch |= m == b"batched";
            }
            if !(got_plain && got_batch) {
                thread::sleep(Duration::from_millis(10));
            }
        }

        let (peer, payload) = b.receive_reliable().expect("reliable inbox");
        assert_eq!(peer, "A");
        assert_eq!(payload, b"important");

        // B replies over the same link.
        b.send_udp(a_addr, b"world").expect("reply");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let msgs = a.poll_incoming_from();
            if msgs.iter().any(|(_, m)| m == b"world") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "A never got reply");
            thread::sleep(Duration::from_millis(10));
        }
        a.shutdown();
        b.shutdown();
    }

    #[test]
    fn webrtc_session_swaps_plain_and_reliable_messages() {
        let signal = spawn_signaling();
        let ice = vec![stun_server()];
        let mut a = NetLink::Webrtc(Box::new(WebRtcSession::new(
            "A",
            signal.clone(),
            ice.clone(),
        )));
        let mut b = NetLink::Webrtc(Box::new(WebRtcSession::new("B", signal, ice)));

        // The WebRTC offer/answer handshake needs both sides connecting at the
        // same time, so run the two requests concurrently.
        let (tx, rx) = std::sync::mpsc::channel();
        let tx_b = tx.clone();
        thread::spawn(move || {
            let r = a.request_connection("B").map(|addr| (addr, a));
            let _ = tx.send(("A", r));
        });
        thread::spawn(move || {
            let r = b.request_connection("A").map(|addr| (addr, b));
            let _ = tx_b.send(("B", r));
        });
        let mut b_addr = None;
        let mut a_addr = None;
        let mut a = None;
        let mut b = None;
        for _ in 0..2 {
            let (name, r) = rx.recv().expect("peer thread panicked");
            let (addr, link) = r.unwrap_or_else(|e| panic!("{name} WebRTC connect failed: {e:#}"));
            if name == "A" {
                b_addr = Some(addr);
                a = Some(link);
            } else {
                a_addr = Some(addr);
                b = Some(link);
            }
        }
        let mut a = a.unwrap();
        let mut b = b.unwrap();
        let b_addr = b_addr.unwrap();
        let a_addr = a_addr.unwrap();

        a.send_udp(b_addr, b"hello").expect("plain send");
        a.send_reliable("B", b"important").expect("reliable send");
        a.batch_send_to_all(b"batched");
        thread::sleep(Duration::from_millis(20));
        a.flush_batches();

        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut got_plain = false;
        let mut got_batch = false;
        while !(got_plain && got_batch) {
            assert!(
                std::time::Instant::now() < deadline,
                "B never received plain/batched messages over WebRTC"
            );
            for (_, m) in b.poll_incoming_from() {
                got_plain |= m == b"hello";
                got_batch |= m == b"batched";
            }
            if !(got_plain && got_batch) {
                thread::sleep(Duration::from_millis(10));
            }
        }

        let (peer, payload) = b.receive_reliable().expect("reliable inbox");
        assert_eq!(peer, "A");
        assert_eq!(payload, b"important");

        b.send_udp(a_addr, b"world").expect("reply");
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let msgs = a.poll_incoming_from();
            if msgs.iter().any(|(_, m)| m == b"world") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "A never got WebRTC reply"
            );
            thread::sleep(Duration::from_millis(10));
        }
        a.shutdown();
        b.shutdown();
    }
}
