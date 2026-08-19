//! Hybrid transport: WebRTC NAT traversal with automatic fallback to direct
//! UDP.
//!
//! Real-world peers live behind NAT, where raw UDP fails. The platform tries
//! WebRTC first (STUN/ICE punch through most NATs); if that cannot establish a
//! link (strict firewall, unreachable STUN server), it falls back to the
//! classic direct-UDP path so the game still connects when WebRTC is
//! unavailable.

use crate::peer_connection::PeerConnection;
use crate::webrtc_connection::{WebRtcConnection, WebRtcStats, HANDSHAKE_TIMEOUT};
use anyhow::{Context, Result};
use std::time::Duration;
use webrtc::ice_transport::ice_server::RTCIceServer;

/// The transport a hybrid link ended up using.
pub enum ConnectionType {
    /// WebRTC data channel — works across most NATs.
    WebRtc(Box<WebRtcConnection>),
    /// Plain UDP socket — the fallback when WebRTC fails.
    DirectUdp(Box<PeerConnection>),
}

/// How a hybrid connection should be established.
#[derive(Debug, Clone)]
pub struct HybridConfig {
    /// Try WebRTC at all. When set, the connection skips straight to UDP.
    pub force_direct_udp: bool,
    /// Budget for the WebRTC handshake before falling back.
    pub webrtc_timeout: Duration,
    /// STUN/TURN servers (defaults to Google's public STUN endpoint).
    pub ice_servers: Option<Vec<RTCIceServer>>,
    /// Signaling address used only by the WebRTC attempt (defaults to the
    /// shared one). Lets callers simulate an unreachable WebRTC signaling path
    /// so the fallback can be exercised deterministically.
    pub webrtc_signal_addr: Option<String>,
}

impl Default for HybridConfig {
    fn default() -> Self {
        Self {
            force_direct_udp: false,
            webrtc_timeout: HANDSHAKE_TIMEOUT,
            ice_servers: None,
            webrtc_signal_addr: None,
        }
    }
}

/// A single-peer transport that prefers WebRTC and falls back to direct UDP.
pub struct HybridConnection {
    remote_id: String,
    connection: ConnectionType,
}

/// Cross-transport statistics for the HUD and performance report.
#[derive(Debug, Clone, Default)]
pub struct HybridStats {
    /// "WebRTC (direct)", "WebRTC (relay)", or "Direct UDP".
    pub connection_type: &'static str,
    /// Latest measured round-trip time (ms).
    pub rtt_ms: f64,
    pub packets_sent: u64,
    pub packets_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub messages_sent: u64,
    pub messages_received: u64,
    pub packet_loss_percent: f64,
    pub bandwidth_kbps: f64,
    pub avg_batch_size: f64,
    /// ICE connection time (ms), when a WebRTC link was used.
    pub ice_connection_time_ms: f64,
}

impl HybridConnection {
    /// Establishes a link to `remote_id`, preferring WebRTC and falling back
    /// to direct UDP on failure.
    ///
    /// `signal_addr` is the signaling server (`"127.0.0.1:9001"`); `udp_bind`
    /// is the local UDP bind used only by the fallback path.
    pub fn establish(
        local_id: &str,
        remote_id: &str,
        signal_addr: &str,
        udp_bind: &str,
        config: &HybridConfig,
    ) -> Result<Self> {
        if !config.force_direct_udp {
            let webrtc_signal = config.webrtc_signal_addr.as_deref().unwrap_or(signal_addr);
            match WebRtcConnection::connect_with_config(
                local_id,
                remote_id,
                webrtc_signal,
                config.webrtc_timeout,
                config.ice_servers.clone(),
            ) {
                Ok(conn) => {
                    println!("[hybrid] {local_id} -> {remote_id}: WebRTC link established");
                    return Ok(Self {
                        remote_id: remote_id.to_string(),
                        connection: ConnectionType::WebRtc(Box::new(conn)),
                    });
                }
                Err(e) => {
                    println!(
                        "[hybrid] {local_id} -> {remote_id}: WebRTC failed ({e:#}) — \
                         falling back to direct UDP"
                    );
                }
            }
        }

        // Fallback: classic direct UDP via the signaling server.
        let mut pc = PeerConnection::new(local_id, signal_addr, udp_bind)
            .with_context(|| "WebRTC failed and direct-UDP fallback could not open")?;
        pc.request_connection(remote_id)?;
        println!("[hybrid] {local_id} -> {remote_id}: direct UDP fallback established");
        Ok(Self {
            remote_id: remote_id.to_string(),
            connection: ConnectionType::DirectUdp(Box::new(pc)),
        })
    }

    /// The remote peer this link connects to.
    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    /// Which transport is currently in use.
    pub fn connection_type(&self) -> &ConnectionType {
        &self.connection
    }

    /// Whether the link is usable.
    pub fn is_connected(&self) -> bool {
        match &self.connection {
            ConnectionType::WebRtc(c) => c.is_connected(),
            ConnectionType::DirectUdp(_) => true,
        }
    }

    /// Sends `data` over the active transport (immediate, unbatched).
    pub fn send(&self, data: &[u8]) -> Result<()> {
        match &self.connection {
            ConnectionType::WebRtc(c) => c.send(data),
            ConnectionType::DirectUdp(pc) => {
                let addr = pc
                    .peer_addr(&self.remote_id)
                    .ok_or_else(|| anyhow::anyhow!("direct-UDP link not established"))?;
                pc.send_udp(*addr, data)?;
                Ok(())
            }
        }
    }

    /// Queues `data` for batched delivery to the remote peer.
    pub fn batch_send(&self, data: &[u8]) {
        match &self.connection {
            ConnectionType::WebRtc(c) => c.batch_send(data),
            ConnectionType::DirectUdp(pc) => pc.batch_send_to_all(data),
        }
    }

    /// Flushes ready batches as one packet/frame. Call once per frame.
    pub fn flush_batches(&self) {
        match &self.connection {
            ConnectionType::WebRtc(c) => c.flush_batches(),
            ConnectionType::DirectUdp(pc) => pc.flush_batches(),
        }
    }

    /// Pings the remote peer (RTT is measured on the next [`Self::poll_received`]).
    pub fn ping(&self) {
        match &self.connection {
            ConnectionType::WebRtc(c) => c.ping(),
            ConnectionType::DirectUdp(pc) => pc.ping_all(),
        }
    }

    /// Drains inbound payloads (batch frames split, ping/pong handled).
    pub fn poll_received(&self) -> Vec<Vec<u8>> {
        match &self.connection {
            ConnectionType::WebRtc(c) => c.poll_received(),
            ConnectionType::DirectUdp(pc) => pc
                .poll_incoming_from()
                .into_iter()
                .map(|(_, p)| p)
                .collect(),
        }
    }

    /// Cross-transport statistics.
    pub fn stats(&self) -> HybridStats {
        let mut s = HybridStats::default();
        match &self.connection {
            ConnectionType::WebRtc(c) => {
                let st: WebRtcStats = c.stats();
                s.connection_type = if st.relayed {
                    "WebRTC (relay)"
                } else {
                    "WebRTC (direct)"
                };
                s.rtt_ms = st.rtt_ms;
                s.packets_sent = st.packets_sent;
                s.packets_received = st.packets_received;
                s.bytes_sent = st.bytes_sent;
                s.bytes_received = st.bytes_received;
                s.messages_sent = st.messages_sent;
                s.messages_received = st.messages_received;
                s.packet_loss_percent = st.packet_loss_percent;
                s.bandwidth_kbps = st.bandwidth_kbps;
                s.avg_batch_size = st.avg_batch_size;
                s.ice_connection_time_ms = st.ice_connection_time_ms;
            }
            ConnectionType::DirectUdp(pc) => {
                let st = pc.stats();
                s.connection_type = "Direct UDP";
                s.rtt_ms = st.rtt_ms;
                s.packets_sent = st.packets_sent;
                s.packets_received = st.packets_received;
                s.bytes_sent = st.bytes_sent;
                s.bytes_received = st.bytes_received;
                s.messages_sent = st.messages_sent;
                s.messages_received = st.messages_received;
                s.packet_loss_percent = st.packet_loss_percent;
                s.bandwidth_kbps = st.bandwidth_kbps;
                s.avg_batch_size = st.avg_batch_size;
            }
        }
        s
    }

    /// Closes the link (gracefully for WebRTC; the UDP socket and signaling
    /// WebSocket close when the connection is dropped).
    pub fn close(&self) -> Result<()> {
        match &self.connection {
            ConnectionType::WebRtc(c) => c.close(),
            ConnectionType::DirectUdp(_) => Ok(()),
        }
    }
}
