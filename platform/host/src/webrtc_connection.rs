//! WebRTC-based NAT traversal for direct peer-to-peer connectivity.
//!
//! Raw UDP sockets (see [`crate::peer_connection`]) work on loopback but fail
//! when peers sit behind NAT/firewalls, which is almost every real-world
//! internet user. A [`WebRtcConnection`] opens an ICE-agented WebRTC data
//! channel instead: STUN discovers the public address, ICE punches through the
//! NAT, and (when direct connectivity is impossible) a TURN relay carries the
//! traffic.
//!
//! # Architecture
//!
//! The heavy lifting runs in a background thread hosting a Tokio runtime (the
//! `webrtc` crate is async). The game-facing facade is synchronous so it drops
//! into the existing per-frame network poll without an async executor:
//!
//! - `connect` starts the background thread and blocks until the data channel
//!   is open (or the handshake times out).
//! - `send` / `batch_send` / `flush_batches` hand already-framed bytes to the
//!   background thread, which writes them over the data channel.
//! - `poll_received` drains inbound bytes, transparently handling ping/pong
//!   (RTT) and batch-frame splitting (loss detection).
//!
//! The SDP offer/answer and ICE candidate exchange happens over a dedicated
//! WebSocket to the signaling server using the `webrtc_offer`,
//! `webrtc_answer`, and `ice_candidate` messages, which the signaling server
//! relays between peers.

use crate::chunk;
use crate::net;
use crate::peer_connection::{BATCH_WINDOW_MS, MAX_BATCH_MESSAGES, PING_TAG, PONG_TAG};
use anyhow::{anyhow, bail, Context, Result};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::{APIBuilder, API};
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::data_channel_state::RTCDataChannelState;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice::mdns::MulticastDnsMode;
use webrtc::ice_transport::ice_candidate::{RTCIceCandidate, RTCIceCandidateInit};
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

/// Google's public STUN server — sufficient for testing and for most NATs.
/// Production deployments should run their own STUN/TURN servers.
pub const DEFAULT_STUN_URL: &str = "stun:stun.l.google.com:19302";

/// Default handshake budget: SDP exchange + ICE + DTLS + SCTP.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// Lifecycle state of the WebRTC link, shared with the game thread.
#[derive(Debug, Clone, PartialEq)]
pub enum WebRtcStatus {
    /// ICE gathering / SDP exchange in progress.
    Connecting,
    /// The data channel is open and game traffic can flow.
    Connected,
    /// The handshake failed or the link died.
    Failed(String),
    /// `close` was called.
    Closed,
}

impl WebRtcStatus {
    /// Human-readable label for logs/HUD.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Failed(_) => "failed",
            Self::Closed => "closed",
        }
    }
}

/// Per-link WebRTC statistics, mirroring [`crate::peer_connection::PeerStats`]
/// plus WebRTC-specific ICE/candidate counters.
#[derive(Debug, Clone, Default)]
pub struct WebRtcStats {
    /// Time from `connect` until the data channel opened (ms).
    pub ice_connection_time_ms: f64,
    /// Latest measured round-trip time over the data channel (ms).
    pub rtt_ms: f64,
    /// Recent RTT samples (ms).
    pub rtt_samples: Vec<f64>,
    /// Data-channel messages sent / received (each is one batched frame).
    pub packets_sent: u64,
    pub packets_received: u64,
    /// Payload bytes sent / received.
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// Inner game messages sent / received (including those inside batches).
    pub messages_sent: u64,
    pub messages_received: u64,
    /// Batched frames actually sent (excludes ping/pong frames).
    pub batch_frames_sent: u64,
    /// ICE candidates gathered locally / received from the peer.
    pub candidates_sent: u64,
    pub candidates_received: u64,
    /// Whether the transport is a TURN relay (true) or a direct link (false).
    pub relayed: bool,
    /// Bandwidth (KiB/s) since the counters were last sampled.
    pub bandwidth_kbps: f64,
    /// Average inner messages per sent data-channel frame.
    pub avg_batch_size: f64,
    /// Estimated packet loss percent (batch sequence gaps over the channel).
    pub packet_loss_percent: f64,
}

/// Shared state between the game-facing facade and the background runtime.
struct Shared {
    state: Arc<Mutex<WebRtcStatus>>,
    inbox: Arc<Mutex<VecDeque<Vec<u8>>>>,
    stats: Arc<Mutex<WebRtcStats>>,
}

/// A synchronous facade over an async WebRTC data channel.
///
/// Only one remote peer per connection (each WebRTC session is a single link);
/// `batch_send` / `flush_batches` accumulate messages and ship them as one
/// batched data-channel frame using the exact framing from Commit 29
/// ([`net::build_batch`]), so the WebRTC path keeps every bandwidth
/// optimization of the UDP path.
pub struct WebRtcConnection {
    peer_id: String,
    remote_id: String,
    data_tx: std_mpsc::Sender<Vec<u8>>,
    shared: Arc<Shared>,
    shutdown: Arc<tokio::sync::Notify>,
    thread: Option<JoinHandle<()>>,
    /// The single pending batch for this link, with when it started.
    pending: Mutex<(Vec<Vec<u8>>, Instant)>,
    /// Monotonic batch sequence number.
    batch_seq: Mutex<u64>,
    /// Last seen batch sequence (for loss detection over the channel).
    batch_seq_seen: Mutex<Option<u32>>,
    packets_lost: Mutex<u64>,
    /// Last `bytes_sent`/`bytes_received` sample for bandwidth.
    bandwidth_sample: Mutex<(Instant, u64, u64)>,
    started_at: Instant,
}

impl WebRtcConnection {
    /// Connects to `remote_id` over WebRTC, using the signaling server at
    /// `signal_addr` (e.g. `"127.0.0.1:9001"`) for SDP/ICE exchange and
    /// Google's public STUN server for NAT discovery.
    ///
    /// Blocks until the data channel opens or `timeout` elapses. The peer with
    /// the lexicographically smaller id is the offerer, so both sides agree on
    /// roles without extra coordination.
    pub fn connect(
        local_id: impl Into<String>,
        remote_id: impl Into<String>,
        signal_addr: &str,
        timeout: Duration,
    ) -> Result<Self> {
        Self::connect_with_config(local_id, remote_id, signal_addr, timeout, None)
    }

    /// Like [`Self::connect`] but with an explicit STUN/TURN server list.
    pub fn connect_with_config(
        local_id: impl Into<String>,
        remote_id: impl Into<String>,
        signal_addr: &str,
        timeout: Duration,
        ice_servers: Option<Vec<RTCIceServer>>,
    ) -> Result<Self> {
        let local_id = local_id.into();
        let remote_id = remote_id.into();

        let shared = Arc::new(Shared {
            state: Arc::new(Mutex::new(WebRtcStatus::Connecting)),
            inbox: Arc::new(Mutex::new(VecDeque::new())),
            stats: Arc::new(Mutex::new(WebRtcStats::default())),
        });
        let (data_tx, data_rx) = std_mpsc::channel::<Vec<u8>>();
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let ice_servers = ice_servers.unwrap_or_else(default_ice_servers);
        // A TURN relay is configured: ICE may fall back to relaying through it
        // when no direct path exists, so report the transport accordingly.
        shared.stats.lock().unwrap().relayed = ice_servers
            .iter()
            .any(|s| s.urls.iter().any(|u| u.starts_with("turn:")));

        let thread = {
            let shared = shared.clone();
            let shutdown = shutdown.clone();
            let params = SessionParams {
                local_id: local_id.clone(),
                remote_id: remote_id.clone(),
                signal_addr: signal_addr.to_string(),
                ice_servers,
                timeout,
            };
            let thread_local = local_id.clone();
            let thread_remote = remote_id.clone();
            std::thread::Builder::new()
                .name(format!("webrtc-{thread_local}->{thread_remote}"))
                .spawn(move || {
                    run_webrtc_thread(params, data_rx, shared, shutdown);
                })
                .context("failed to spawn WebRTC runtime thread")?
        };

        let conn = Self {
            peer_id: local_id,
            remote_id: remote_id.clone(),
            data_tx,
            shared,
            shutdown,
            thread: Some(thread),
            pending: Mutex::new((Vec::new(), Instant::now())),
            batch_seq: Mutex::new(0),
            batch_seq_seen: Mutex::new(None),
            packets_lost: Mutex::new(0),
            bandwidth_sample: Mutex::new((Instant::now(), 0, 0)),
            started_at: Instant::now(),
        };

        // Block until the data channel is open (or the handshake fails).
        let deadline = Instant::now() + timeout + Duration::from_secs(2);
        loop {
            match conn.status() {
                WebRtcStatus::Connected => {
                    let elapsed = conn.started_at.elapsed().as_secs_f64() * 1000.0;
                    conn.shared.stats.lock().unwrap().ice_connection_time_ms = elapsed;
                    return Ok(conn);
                }
                WebRtcStatus::Failed(e) => bail!("WebRTC connection failed: {e}"),
                _ => {}
            }
            if Instant::now() >= deadline {
                let _ = conn.close();
                bail!("WebRTC connection to '{remote_id}' timed out (no direct path / STUN unreachable)");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// This node's peer id.
    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    /// The remote peer this link connects to.
    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    /// Whether the data channel is open.
    pub fn is_connected(&self) -> bool {
        self.status() == WebRtcStatus::Connected
    }

    /// Current lifecycle status.
    pub fn status(&self) -> WebRtcStatus {
        self.shared.state.lock().unwrap().clone()
    }

    /// Sends `data` as a single data-channel message (immediate, unbatched).
    pub fn send(&self, data: &[u8]) -> Result<()> {
        self.data_tx
            .send(data.to_vec())
            .map_err(|_| anyhow!("WebRTC runtime is not running"))
    }

    /// Queues `data` for **batched** delivery to the remote peer. Batches are
    /// flushed by [`Self::flush_batches`] after up to 16 ms (or once they hold
    /// 10 messages), so high-frequency traffic ships as few frames.
    pub fn batch_send(&self, data: &[u8]) {
        let mut pending = self.pending.lock().unwrap();
        pending.0.push(data.to_vec());
    }

    /// Flushes the pending batch as one batched data-channel frame if the
    /// window elapsed or the batch is full. Call once per frame.
    pub fn flush_batches(&self) {
        let now = Instant::now();
        let ready: Option<Vec<Vec<u8>>> = {
            let mut pending = self.pending.lock().unwrap();
            let (msgs, start) = &mut *pending;
            let due = msgs.len() >= MAX_BATCH_MESSAGES
                || now.duration_since(*start) >= Duration::from_millis(BATCH_WINDOW_MS);
            if due && !msgs.is_empty() {
                Some(std::mem::take(msgs))
            } else {
                None
            }
        };
        if let Some(msgs) = ready {
            let mut seq = self.batch_seq.lock().unwrap();
            *seq += 1;
            let packet = net::build_batch(&msgs, *seq as u32);
            drop(seq);
            if self.send(&packet).is_ok() {
                let mut stats = self.shared.stats.lock().unwrap();
                stats.messages_sent += msgs.len() as u64;
                stats.batch_frames_sent += 1;
            }
        }
    }

    /// Pings the remote peer; the pong is measured on the next
    /// [`Self::poll_received`] and recorded as RTT.
    pub fn ping(&self) {
        let mut buf = [0u8; 17];
        buf[0] = PING_TAG;
        buf[1..9].copy_from_slice(&chunk::now_ms().to_le_bytes());
        let nonce: u64 = chunk::now_ms().wrapping_mul(0x9E37_79B9);
        buf[9..17].copy_from_slice(&nonce.to_le_bytes());
        let _ = self.send(&buf);
    }

    /// Drains inbound data-channel payloads. Ping frames are echoed back as
    /// pongs (same timestamp, so the remote can measure RTT); pong frames are
    /// measured here; batch frames are split into their inner messages with
    /// sequence-gap loss detection.
    pub fn poll_received(&self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut drained = {
            let mut inbox = self.shared.inbox.lock().unwrap();
            std::mem::take(&mut *inbox)
        };
        for payload in drained.drain(..) {
            match payload.first() {
                Some(&PING_TAG) if payload.len() == 17 => {
                    // Echo a pong carrying the same timestamp.
                    let mut pong = [0u8; 17];
                    pong[0] = PONG_TAG;
                    pong[1..].copy_from_slice(&payload[1..17]);
                    let _ = self.send(&pong);
                }
                Some(&PONG_TAG) if payload.len() == 17 => {
                    let sent_ms = u64::from_le_bytes(payload[1..9].try_into().unwrap());
                    let rtt = chunk::now_ms().saturating_sub(sent_ms) as f64;
                    let mut stats = self.shared.stats.lock().unwrap();
                    stats.rtt_ms = rtt;
                    stats.rtt_samples.push(rtt);
                    if stats.rtt_samples.len() > 1000 {
                        stats.rtt_samples.remove(0);
                    }
                }
                Some(&net::BATCH_TAG) => {
                    if let Some((seq, msgs)) = net::split_batch(&payload) {
                        self.note_batch(seq);
                        {
                            let mut stats = self.shared.stats.lock().unwrap();
                            stats.messages_received += msgs.len() as u64;
                            stats.packets_received += 1;
                        }
                        for m in msgs {
                            out.push(m);
                        }
                    }
                }
                _ => out.push(payload),
            }
        }
        out
    }

    /// Records batch arrival for loss detection.
    fn note_batch(&self, seq: u32) {
        let mut seen = self.batch_seq_seen.lock().unwrap();
        if let Some(prev) = *seen {
            let expected = prev.wrapping_add(1);
            if seq != expected {
                let lost = seq.wrapping_sub(expected) as u64;
                *self.packets_lost.lock().unwrap() += lost.min(1000);
            }
        }
        *seen = Some(seq);
    }

    /// Per-link statistics, including derived loss/bandwidth and average batch
    /// size.
    pub fn stats(&self) -> WebRtcStats {
        let mut stats = self.shared.stats.lock().unwrap().clone();
        let lost = *self.packets_lost.lock().unwrap();
        let total = stats.packets_received + lost;
        stats.packet_loss_percent = if total > 0 {
            lost as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        stats.avg_batch_size = stats.messages_sent as f64 / stats.batch_frames_sent.max(1) as f64;
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

    /// Average RTT over the collected samples, if any.
    pub fn avg_rtt_ms(&self) -> Option<f64> {
        let stats = self.shared.stats.lock().unwrap();
        if stats.rtt_samples.is_empty() {
            None
        } else {
            Some(stats.rtt_samples.iter().sum::<f64>() / stats.rtt_samples.len() as f64)
        }
    }

    /// Signals the background runtime to shut down and closes the data channel
    /// and peer connection.
    pub fn close(&self) -> Result<()> {
        self.shutdown.notify_one();
        Ok(())
    }

    /// Joins the background runtime thread (best-effort; the thread exits
    /// shortly after [`Self::close`]).
    pub fn join(mut self) {
        let _ = self.close();
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for WebRtcConnection {
    fn drop(&mut self) {
        self.shutdown.notify_one();
    }
}

/// The default ICE server list: Google's public STUN endpoint. A TURN relay is
/// intentionally omitted — see `docs` for production TURN deployment.
fn default_ice_servers() -> Vec<RTCIceServer> {
    vec![RTCIceServer {
        urls: vec![DEFAULT_STUN_URL.to_string()],
        ..Default::default()
    }]
}

/// Everything the background runtime needs to drive one WebRTC session.
struct SessionParams {
    local_id: String,
    remote_id: String,
    signal_addr: String,
    ice_servers: Vec<RTCIceServer>,
    timeout: Duration,
}

/// Entry point for the background thread: builds a Tokio runtime and drives
/// the WebRTC handshake + data channel. Sets the shared status on completion
/// or failure.
fn run_webrtc_thread(
    params: SessionParams,
    data_rx: std_mpsc::Receiver<Vec<u8>>,
    shared: Arc<Shared>,
    shutdown: Arc<tokio::sync::Notify>,
) {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            *shared.state.lock().unwrap() =
                WebRtcStatus::Failed(format!("could not start Tokio runtime: {e}"));
            return;
        }
    };
    let result = rt.block_on(webrtc_main(&params, data_rx, shared.clone(), shutdown));
    if let Err(e) = result {
        *shared.state.lock().unwrap() = WebRtcStatus::Failed(format!("{e:#}"));
    }
}

/// The async WebRTC session: signaling + ICE + data channel. Returns once the
/// link is closed (or fails during setup).
async fn webrtc_main(
    params: &SessionParams,
    data_rx: std_mpsc::Receiver<Vec<u8>>,
    shared: Arc<Shared>,
    shutdown: Arc<tokio::sync::Notify>,
) -> Result<()> {
    let local_id = &params.local_id;
    let remote_id = &params.remote_id;
    let signal_addr = &params.signal_addr;
    let ice_servers = &params.ice_servers;
    let timeout = params.timeout;
    // --- Signaling WebSocket (SDP offers/answers + ICE candidates). ---
    let url = format!("ws://{signal_addr}/");
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .with_context(|| format!("signaling server at {signal_addr} not reachable"))?;
    let (mut ws_writer, mut ws_reader) = ws.split();

    ws_writer
        .send(WsMessage::text(
            json!({ "type": "register", "peer_id": local_id, "address": "" }).to_string(),
        ))
        .await
        .context("failed to register WebRTC signaling connection")?;

    let (sig_tx, mut sig_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let reader = tokio::spawn(async move {
        while let Some(msg) = ws_reader.next().await {
            match msg {
                Ok(WsMessage::Text(t)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        let _ = sig_tx.send(v);
                    }
                }
                Ok(WsMessage::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    });

    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        while let Some(v) = out_rx.recv().await {
            if ws_writer
                .send(WsMessage::text(v.to_string()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // --- Peer connection with the public STUN server for NAT discovery. ---
    let mut setting_engine = SettingEngine::default();
    setting_engine.set_ice_multicast_dns_mode(MulticastDnsMode::Disabled);
    let api: API = APIBuilder::new()
        .with_setting_engine(setting_engine)
        .build();
    let config = RTCConfiguration {
        ice_servers: ice_servers.to_vec(),
        ..Default::default()
    };
    let pc = Arc::new(api.new_peer_connection(config).await?);

    // Forward every locally-gathered ICE candidate to the remote peer.
    {
        let out = out_tx.clone();
        let from = local_id.to_string();
        let to = remote_id.to_string();
        let shared_c = shared.clone();
        pc.on_ice_candidate(Box::new(move |c: Option<RTCIceCandidate>| {
            let out = out.clone();
            let from = from.clone();
            let to = to.clone();
            let shared_c = shared_c.clone();
            Box::pin(async move {
                if let Some(cand) = c {
                    if let Ok(init) = cand.to_json() {
                        if let Ok(candidate_json) = serde_json::to_string(&init) {
                            let _ = out.send(json!({
                                "type": "ice_candidate",
                                "from_peer": from,
                                "to_peer": to,
                                "candidate": candidate_json,
                            }));
                            shared_c.stats.lock().unwrap().candidates_sent += 1;
                        }
                    }
                }
            })
        }));
    }

    // Inbound signaling: deliver offers/answers to the handshake and feed ICE
    // candidates into the peer connection as they arrive.
    let (offer_tx, mut offer_rx) = tokio::sync::mpsc::unbounded_channel::<RTCSessionDescription>();
    let (answer_tx, mut answer_rx) =
        tokio::sync::mpsc::unbounded_channel::<RTCSessionDescription>();
    {
        let pc_sig = pc.clone();
        let me = local_id.to_string();
        let shared_sig = shared.clone();
        // Detached task: runs until the session ends (dropping the JoinHandle
        // only unlinks it, the task keeps draining inbound signaling).
        tokio::spawn(async move {
            while let Some(v) = sig_rx.recv().await {
                let to_me = v["to_peer"].as_str() == Some(me.as_str());
                if !to_me {
                    continue;
                }
                match v["type"].as_str() {
                    Some("webrtc_offer") => {
                        if let Some(sdp) = v["sdp"].as_str() {
                            if let Ok(desc) = RTCSessionDescription::offer(sdp.to_string()) {
                                let _ = offer_tx.send(desc);
                            }
                        }
                    }
                    Some("webrtc_answer") => {
                        if let Some(sdp) = v["sdp"].as_str() {
                            if let Ok(desc) = RTCSessionDescription::answer(sdp.to_string()) {
                                let _ = answer_tx.send(desc);
                            }
                        }
                    }
                    Some("ice_candidate") => {
                        if let Some(candidate_json) = v["candidate"].as_str() {
                            if let Ok(init) =
                                serde_json::from_str::<RTCIceCandidateInit>(candidate_json)
                            {
                                let _ = pc_sig.add_ice_candidate(init).await;
                                shared_sig.stats.lock().unwrap().candidates_received += 1;
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    // Send a signaling message to the remote peer.
    let send_signal =
        |out: &tokio::sync::mpsc::UnboundedSender<Value>, msg_type: &str, sdp: &str| {
            out.send(json!({
                "type": msg_type,
                "from_peer": local_id,
                "to_peer": remote_id,
                "sdp_type": msg_type.trim_start_matches("webrtc_"),
                "sdp": sdp,
            }))
            .map_err(|_| anyhow!("signaling writer is gone"))
        };

    // --- Offer/answer handshake. The smaller peer id is the offerer. ---
    let is_initiator = local_id < remote_id;
    let dc: Arc<RTCDataChannel> = if is_initiator {
        let channel = pc
            .create_data_channel(
                "game",
                Some(RTCDataChannelInit {
                    ordered: Some(false),
                    max_retransmits: Some(0),
                    ..Default::default()
                }),
            )
            .await?;
        let offer = pc.create_offer(None).await?;
        pc.set_local_description(offer.clone()).await?;
        send_signal(&out_tx, "webrtc_offer", &offer.sdp)?;
        let answer = tokio::time::timeout(timeout, answer_rx.recv())
            .await
            .context("timed out waiting for the WebRTC answer")?
            .ok_or_else(|| anyhow!("WebRTC answer channel closed"))?;
        pc.set_remote_description(answer).await?;
        channel
    } else {
        // The answerer must be listening for the incoming channel before the
        // SCTP session is established.
        let (dc_tx, mut dc_rx) = tokio::sync::mpsc::unbounded_channel::<Arc<RTCDataChannel>>();
        pc.on_data_channel(Box::new(move |channel: Arc<RTCDataChannel>| {
            let t = dc_tx.clone();
            Box::pin(async move {
                let _ = t.send(channel);
            })
        }));
        let offer = tokio::time::timeout(timeout, offer_rx.recv())
            .await
            .context("timed out waiting for the WebRTC offer")?
            .ok_or_else(|| anyhow!("WebRTC offer channel closed"))?;
        pc.set_remote_description(offer).await?;
        let answer = pc.create_answer(None).await?;
        pc.set_local_description(answer.clone()).await?;
        send_signal(&out_tx, "webrtc_answer", &answer.sdp)?;
        tokio::time::timeout(timeout, dc_rx.recv())
            .await
            .context("timed out waiting for the remote data channel")?
            .ok_or_else(|| anyhow!("remote data channel sender dropped"))?
    };

    // Deliver inbound data to the game thread (ping/pong and batch splitting
    // are handled on the game thread by `poll_received`).
    {
        let inbox = shared.inbox.clone();
        let stats = shared.stats.clone();
        dc.on_message(Box::new(move |msg: DataChannelMessage| {
            let inbox = inbox.clone();
            let stats = stats.clone();
            Box::pin(async move {
                let len = msg.data.len();
                inbox.lock().unwrap().push_back(msg.data.to_vec());
                let mut st = stats.lock().unwrap();
                st.packets_received += 1;
                st.bytes_received += len as u64;
            })
        }));
    }

    let dc_for_open = dc.clone();
    let state = shared.state.clone();
    dc.on_open(Box::new(move || {
        let state = state.clone();
        Box::pin(async move {
            *state.lock().unwrap() = WebRtcStatus::Connected;
        })
    }));

    // Wait for the data channel to open (ICE + DTLS + SCTP setup).
    let connected = {
        let mut connected = false;
        let deadline = Instant::now() + timeout;
        loop {
            if dc.ready_state() == RTCDataChannelState::Open {
                connected = true;
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        connected
    };
    if !connected {
        bail!("WebRTC data channel did not open within {timeout:?}");
    }
    *shared.state.lock().unwrap() = WebRtcStatus::Connected;

    // Bridge the game thread's synchronous data queue into the async channel.
    let (tokio_data_tx, mut tokio_data_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1024);
    let bridge = tokio::task::spawn_blocking(move || {
        while let Ok(data) = data_rx.recv() {
            if tokio_data_tx.blocking_send(data).is_err() {
                break;
            }
        }
    });

    // --- Data channel send loop (relay + ping/pong + game traffic). ---
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            data = tokio_data_rx.recv() => {
                match data {
                    Some(data) => {
                        if dc.ready_state() != RTCDataChannelState::Open {
                            continue;
                        }
                        if let Ok(n) = dc.send(&Bytes::from(data)).await {
                            let mut st = shared.stats.lock().unwrap();
                            st.packets_sent += 1;
                            st.bytes_sent += n as u64;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    // Graceful drain: flush any frames queued before the shutdown signal so a
    // racing `close()` never drops the final batch.
    while dc.ready_state() == RTCDataChannelState::Open {
        match tokio_data_rx.try_recv() {
            Ok(data) => {
                if let Ok(n) = dc.send(&Bytes::from(data)).await {
                    let mut st = shared.stats.lock().unwrap();
                    st.packets_sent += 1;
                    st.bytes_sent += n as u64;
                }
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
        }
    }

    let _ = dc_for_open;
    let _ = dc.close().await;
    let _ = pc.close().await;
    let _ = reader.await;
    let _ = writer.await;
    let _ = bridge.await;
    Ok(())
}
