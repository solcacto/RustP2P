//! Network codec: delta-compressed avatar poses, batched packet framing, and
//! reliable message frames.
//!
//! - Avatar poses are sent as 8-byte **deltas** (4×i16, mm precision) instead
//!   of 16-byte full f32 poses, halving pose bandwidth. Full poses are sent for
//!   the first message and on teleports (delta overflow).
//! - Multiple inner messages are accumulated into a single **batch** datagram
//!   (one per peer per flush cycle) to reduce UDP packet count.
//! - Reliable messages carry a sequence number and are acked; the sender
//!   retries until acknowledged.

use crate::avatar_state::{AvatarPose, AvatarState};

/// Tag marking a batch of messages (inner payloads follow).
pub const BATCH_TAG: u8 = 0x11;
/// Tag marking a reliable message frame (`u32 seq` + payload).
pub const RELIABLE_TAG: u8 = 0x12;
/// Tag marking an acknowledgement (`u32 seq`).
pub const ACK_TAG: u8 = 0x13;

/// Full pose payload length (four little-endian f32s).
pub const POSE_FULL_LEN: usize = 16;
/// Delta pose payload length (four little-endian i16s, mm precision).
pub const POSE_DELTA_LEN: usize = 8;

/// Maximum distance (world units) a delta can represent before overflowing.
const DELTA_MAX_UNITS: f32 = 32.0;

/// Encodes a pose for the wire: an 8-byte delta from `prev` when possible,
/// otherwise a 16-byte full pose (first pose or large jump).
pub fn encode_pose(prev: Option<&AvatarState>, cur: &AvatarState) -> Vec<u8> {
    if let Some(prev) = prev {
        let dx = cur.x - prev.x;
        let dy = cur.y - prev.y;
        let dz = cur.z - prev.z;
        let drot = cur.rot_y - prev.rot_y;
        if dx.abs() < DELTA_MAX_UNITS
            && dy.abs() < DELTA_MAX_UNITS
            && dz.abs() < DELTA_MAX_UNITS
            && drot.abs() < DELTA_MAX_UNITS
        {
            let mut out = Vec::with_capacity(POSE_DELTA_LEN);
            out.extend_from_slice(&((dx * 1000.0) as i16).to_le_bytes());
            out.extend_from_slice(&((dy * 1000.0) as i16).to_le_bytes());
            out.extend_from_slice(&((dz * 1000.0) as i16).to_le_bytes());
            out.extend_from_slice(&((drot * 1000.0) as i16).to_le_bytes());
            return out;
        }
    }
    let mut out = Vec::with_capacity(POSE_FULL_LEN);
    out.extend_from_slice(&cur.x.to_le_bytes());
    out.extend_from_slice(&cur.y.to_le_bytes());
    out.extend_from_slice(&cur.z.to_le_bytes());
    out.extend_from_slice(&cur.rot_y.to_le_bytes());
    out
}

/// Decodes a pose payload (8-byte delta or 16-byte full) using the previous
/// pose for delta reconstruction.
pub fn decode_pose(prev: Option<&AvatarPose>, payload: &[u8]) -> Option<AvatarPose> {
    match payload.len() {
        POSE_DELTA_LEN => {
            let prev = prev?;
            let dx = i16::from_le_bytes(payload[0..2].try_into().ok()?);
            let dy = i16::from_le_bytes(payload[2..4].try_into().ok()?);
            let dz = i16::from_le_bytes(payload[4..6].try_into().ok()?);
            let drot = i16::from_le_bytes(payload[6..8].try_into().ok()?);
            Some(AvatarPose {
                x: prev.x + dx as f32 / 1000.0,
                y: prev.y + dy as f32 / 1000.0,
                z: prev.z + dz as f32 / 1000.0,
                rot_y: prev.rot_y + drot as f32 / 1000.0,
                last_seen: std::time::Instant::now(),
            })
        }
        POSE_FULL_LEN => {
            let x = f32::from_le_bytes(payload[0..4].try_into().ok()?);
            let y = f32::from_le_bytes(payload[4..8].try_into().ok()?);
            let z = f32::from_le_bytes(payload[8..12].try_into().ok()?);
            let rot_y = f32::from_le_bytes(payload[12..16].try_into().ok()?);
            if !(x.is_finite() && y.is_finite() && z.is_finite() && rot_y.is_finite()) {
                return None;
            }
            Some(AvatarPose { x, y, z, rot_y, last_seen: std::time::Instant::now() })
        }
        _ => None,
    }
}

/// Builds a batch datagram from inner payloads: `[BATCH_TAG][count u8][seq u32]
/// + count × `[len u8][payload]`.
pub fn build_batch(messages: &[Vec<u8>], seq: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + messages.len() * 8);
    out.push(BATCH_TAG);
    out.push(messages.len().min(255) as u8);
    out.extend_from_slice(&seq.to_le_bytes());
    for m in messages {
        out.push(m.len().min(255) as u8);
        out.extend_from_slice(m);
    }
    out
}

/// Splits a batch datagram into its inner payloads and sequence number.
/// Returns `None` if the datagram is not a batch or is malformed.
pub fn split_batch(payload: &[u8]) -> Option<(u32, Vec<Vec<u8>>)> {
    if payload.first() != Some(&BATCH_TAG) {
        return None;
    }
    let count = *payload.get(1)? as usize;
    let seq = u32::from_le_bytes(payload[2..6].try_into().ok()?);
    let mut pos = 6;
    let mut msgs = Vec::with_capacity(count);
    for _ in 0..count {
        let len = *payload.get(pos)? as usize;
        pos += 1;
        let end = pos + len;
        if end > payload.len() {
            return None;
        }
        msgs.push(payload[pos..end].to_vec());
        pos = end;
    }
    Some((seq, msgs))
}

/// Builds a reliable message frame: `` `[RELIABLE_TAG][seq u32][payload]` ``.
pub fn build_reliable(seq: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(RELIABLE_TAG);
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Builds an acknowledgement frame: `[ACK_TAG][seq u32]`.
pub fn build_ack(seq: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(5);
    out.push(ACK_TAG);
    out.extend_from_slice(&seq.to_le_bytes());
    out
}

/// Parses a reliable frame's sequence number.
pub fn reliable_seq(payload: &[u8]) -> Option<u32> {
    if payload.first() == Some(&RELIABLE_TAG) {
        u32::from_le_bytes(payload.get(1..5)?.try_into().ok()?).into()
    } else {
        None
    }
}

/// Parses an ack frame's sequence number.
pub fn ack_seq(payload: &[u8]) -> Option<u32> {
    if payload.first() == Some(&ACK_TAG) {
        u32::from_le_bytes(payload.get(1..5)?.try_into().ok()?).into()
    } else {
        None
    }
}

/// The reliable payload (everything after the seq) of a reliable frame.
pub fn reliable_payload(payload: &[u8]) -> Option<&[u8]> {
    if payload.first() == Some(&RELIABLE_TAG) && payload.len() > 5 {
        Some(&payload[5..])
    } else {
        None
    }
}

/// A message awaiting acknowledgement, with retry bookkeeping.
#[derive(Debug, Clone)]
pub struct ReliableMessage {
    /// Monotonic sequence number.
    pub sequence_number: u32,
    /// The payload being delivered reliably.
    pub payload: Vec<u8>,
    /// Whether the peer has acknowledged it.
    pub ack_received: bool,
    /// Time to resend if still unacknowledged.
    pub next_resend: std::time::Instant,
    /// Number of retries performed.
    pub retry_count: u32,
}
