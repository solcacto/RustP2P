//! The spatial chunk system: the world is divided into a grid of chunks and
//! each chunk is "hosted" (owned) by a peer.
//!
//! A peer claims a rectangular region of chunks and broadcasts the claim over
//! the P2P mesh. A kad-style distributed hash table maps chunk coordinates to
//! the hosting peer's id and address, with Kademlia's XOR-distance routing for
//! "who is nearest to chunk X?" queries. (Implemented over the platform's UDP
//! mesh rather than the libp2p-kad crate, but with the same semantics.)

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Instant;

/// World-space size of one chunk (world units per chunk edge).
pub const CHUNK_SIZE: f32 = 100.0;

/// Wire tag marking a chunk-claim datagram (claims are larger than 16 bytes,
/// so they can never be confused with poses/scores).
pub const CLAIM_TAG: u8 = 0x01;
/// Wire tag marking a chunk-state pointer datagram.
pub const STATE_TAG: u8 = 0x02;
/// Wire tag marking a zone-join request (entering a peer's zone).
pub const ZONE_JOIN_TAG: u8 = 0x03;
/// Wire tag marking a zone state (a peer's live chunk state).
pub const ZONE_STATE_TAG: u8 = 0x04;

/// Local directory (inside the host crate) where owned chunk states persist.
pub const CHUNKS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/chunks");

/// A position in the chunk grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChunkCoord {
    /// Chunk column (world X / [`CHUNK_SIZE`], floored).
    pub x: i32,
    /// Chunk row (world Z / [`CHUNK_SIZE`], floored).
    pub z: i32,
}

impl ChunkCoord {
    /// Maps a world position to its chunk.
    pub fn at(x: f32, z: f32) -> Self {
        Self {
            x: (x / CHUNK_SIZE).floor() as i32,
            z: (z / CHUNK_SIZE).floor() as i32,
        }
    }

    /// A 64-bit key for XOR-distance routing (sign-preserving bit layout).
    pub fn key(&self) -> u64 {
        ((self.x as i64 as u64) << 32) | (self.z as u32 as u64)
    }

    /// Kademlia XOR distance between two chunk coordinates.
    pub fn distance_xor(&self, other: &ChunkCoord) -> u64 {
        self.key() ^ other.key()
    }
}

/// A peer's claim that it hosts a rectangular region of chunks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkClaim {
    /// The hosting peer's id.
    pub peer_id: String,
    /// Origin chunk of the claimed region.
    pub origin_x: i32,
    pub origin_z: i32,
    /// Region size in chunks along X and Z (at least 1 each).
    pub extent_x: u32,
    pub extent_z: u32,
}

impl ChunkClaim {
    /// A claim that this peer hosts chunks `(X, Y)` and `(X+1, Y)`.
    pub fn region(peer_id: impl Into<String>, origin: ChunkCoord, extent: (u32, u32)) -> Self {
        Self {
            peer_id: peer_id.into(),
            origin_x: origin.x,
            origin_z: origin.z,
            extent_x: extent.0.max(1),
            extent_z: extent.1.max(1),
        }
    }

    /// The origin chunk coordinate.
    pub fn origin(&self) -> ChunkCoord {
        ChunkCoord { x: self.origin_x, z: self.origin_z }
    }

    /// Whether this claim covers `coord`.
    pub fn covers(&self, coord: ChunkCoord) -> bool {
        coord.x >= self.origin_x
            && coord.x < self.origin_x + self.extent_x as i32
            && coord.z >= self.origin_z
            && coord.z < self.origin_z + self.extent_z as i32
    }

    /// Every chunk covered by this claim.
    pub fn coords(&self) -> Vec<ChunkCoord> {
        let mut out = Vec::with_capacity((self.extent_x * self.extent_z) as usize);
        for x in 0..self.extent_x as i32 {
            for z in 0..self.extent_z as i32 {
                out.push(ChunkCoord {
                    x: self.origin_x + x,
                    z: self.origin_z + z,
                });
            }
        }
        out
    }

    /// Serializes the claim to a wire datagram: `[CLAIM_TAG] + JSON`.
    pub fn wire_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        out.push(CLAIM_TAG);
        out.extend_from_slice(&serde_json::to_vec(self).unwrap_or_default());
        out
    }

    /// Parses a wire datagram into a claim, if it carries one.
    pub fn from_wire(bytes: &[u8]) -> Option<Self> {
        if bytes.first() != Some(&CLAIM_TAG) {
            return None;
        }
        serde_json::from_slice(&bytes[1..]).ok()
    }
}

/// A chunk→host entry in the distributed hash table.
#[derive(Debug, Clone)]
pub struct ChunkEntry {
    /// The hosting peer's id.
    pub peer_id: String,
    /// The hosting peer's UDP address.
    pub address: SocketAddr,
    /// The owner's identity key (`ed25519:<hex>`), if known.
    pub owner_pubkey: Option<String>,
    /// CID of the chunk's persisted state cached on IPFS, if published.
    pub state_cid: Option<String>,
    /// When the entry was last confirmed by a claim.
    pub last_seen: Instant,
}

/// A kad-style DHT mapping chunk coordinates to hosting peers.
///
/// Uses Kademlia's XOR-distance metric for "nearest chunk" routing: the table
/// keeps one entry per chunk and can answer both exact lookups and nearest-
/// neighbor queries ordered by XOR distance to a target coordinate.
#[derive(Debug, Default)]
pub struct ChunkDht {
    entries: HashMap<ChunkCoord, ChunkEntry>,
}

impl ChunkDht {
    /// Creates an empty chunk DHT.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records (or refreshes) that `peer_id` at `address` hosts `coord`.
    pub fn put(&mut self, coord: ChunkCoord, peer_id: impl Into<String>, address: SocketAddr) {
        self.entries.insert(
            coord,
            ChunkEntry {
                peer_id: peer_id.into(),
                address,
                owner_pubkey: None,
                state_cid: None,
                last_seen: Instant::now(),
            },
        );
    }

    /// Records the owner's identity key for a chunk.
    pub fn record_owner(&mut self, coord: ChunkCoord, owner_pubkey: impl Into<String>) {
        if let Some(e) = self.entries.get_mut(&coord) {
            e.owner_pubkey = Some(owner_pubkey.into());
        }
    }

    /// Records the IPFS CID of a chunk's persisted state (the "ruins" cache),
    /// keyed by the chunk coordinate + owner key. Creating the entry with the
    /// owner's details if it is not yet present.
    pub fn record_state(
        &mut self,
        coord: ChunkCoord,
        state_cid: impl Into<String>,
        owner_pubkey: impl Into<String>,
        peer_id: impl Into<String>,
        address: SocketAddr,
    ) {
        let entry = self.entries.entry(coord).or_insert_with(|| ChunkEntry {
            peer_id: peer_id.into(),
            address,
            owner_pubkey: None,
            state_cid: None,
            last_seen: Instant::now(),
        });
        entry.state_cid = Some(state_cid.into());
        entry.owner_pubkey = Some(owner_pubkey.into());
    }

    /// Applies every chunk of a claim.
    pub fn apply_claim(&mut self, claim: &ChunkClaim, address: SocketAddr) {
        for coord in claim.coords() {
            self.put(coord, claim.peer_id.clone(), address);
        }
    }

    /// Looks up who hosts `coord`.
    pub fn get(&self, coord: ChunkCoord) -> Option<&ChunkEntry> {
        self.entries.get(&coord)
    }

    /// Removes every entry hosted by `peer_id` (e.g. on disconnect).
    pub fn remove_peer(&mut self, peer_id: &str) {
        self.entries.retain(|_, e| e.peer_id != peer_id);
    }

    /// The `k` chunks closest to `target` by XOR distance (kademlia routing).
    pub fn nearest(&self, target: ChunkCoord, k: usize) -> Vec<(ChunkCoord, u64)> {
        let mut all: Vec<(ChunkCoord, u64)> = self
            .entries
            .keys()
            .map(|c| (*c, c.distance_xor(&target)))
            .collect();
        all.sort_by_key(|(_, d)| *d);
        all.truncate(k);
        all
    }

    /// Number of known chunk→host mappings.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A single modification a peer makes to a chunk it owns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkEdit {
    /// World-space X of the modification.
    pub x: f32,
    /// World-space Z of the modification.
    pub z: f32,
    /// What was modified, e.g. `"ruin"`, `"structure"`, `"block"`.
    pub kind: String,
    /// A scalar describing the modification (height, scale, count...).
    pub value: f32,
}

/// The persisted state of one chunk, owned by a peer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkState {
    /// The chunk this state describes.
    pub chunk: ChunkCoord,
    /// The owning peer's id.
    pub owner: String,
    /// The owning peer's identity key (`ed25519:<hex>`); part of the IPFS key.
    pub owner_pubkey: String,
    /// Unix milliseconds of the last modification.
    pub modified_at: u64,
    /// The modifications (ruins/structures) in this chunk.
    pub edits: Vec<ChunkEdit>,
}

impl ChunkState {
    /// Applies an edit, updating the modification timestamp.
    pub fn apply(&mut self, edit: ChunkEdit) {
        self.edits.push(edit);
        self.modified_at = now_ms();
    }
}

/// A pointer to a chunk's state cached on IPFS, broadcast when the owning peer
/// goes offline so others can load the "ruins" in its absence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkStatePointer {
    /// The peer that published the state.
    pub peer_id: String,
    /// The owner's identity key (`ed25519:<hex>`).
    pub owner_pubkey: String,
    /// The chunk the state describes.
    pub chunk_x: i32,
    pub chunk_z: i32,
    /// The IPFS CID of the persisted [`ChunkState`].
    pub state_cid: String,
}

impl ChunkStatePointer {
    /// The IPFS cache key for a chunk state: chunk coordinate + owner key.
    pub fn key(&self) -> String {
        format!("chunk_{}_{}_{}", self.chunk_x, self.chunk_z, self.owner_pubkey)
    }

    /// Serializes to a wire datagram: `[STATE_TAG] + JSON`.
    pub fn wire_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(96);
        out.push(STATE_TAG);
        out.extend_from_slice(&serde_json::to_vec(self).unwrap_or_default());
        out
    }

    /// Parses a wire datagram into a pointer, if it carries one.
    pub fn from_wire(bytes: &[u8]) -> Option<Self> {
        if bytes.first() != Some(&STATE_TAG) {
            return None;
        }
        serde_json::from_slice(&bytes[1..]).ok()
    }
}

/// Local persistence for the chunks this peer owns (plain JSON files).
pub struct ChunkStore {
    dir: PathBuf,
}

impl ChunkStore {
    /// A store for `peer_id`, rooted at `CHUNKS_DIR/<peer_id>`.
    pub fn new(peer_id: &str) -> Self {
        Self { dir: PathBuf::from(CHUNKS_DIR).join(peer_id) }
    }

    /// Writes `state` to `chunk_<x>_<z>.json`.
    pub fn save(&self, state: &ChunkState) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(format!("chunk_{}_{}.json", state.chunk.x, state.chunk.z));
        std::fs::write(&path, serde_json::to_vec_pretty(state)?)
            .with_context(|| format!("cannot save chunk state to {}", path.display()))?;
        Ok(())
    }

    /// Loads every chunk state this peer has saved.
    pub fn load_all(&self) -> Result<Vec<ChunkState>> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                if let Ok(bytes) = std::fs::read(entry.path()) {
                    if let Ok(state) = serde_json::from_slice(&bytes) {
                        out.push(state);
                    }
                }
            }
        }
        Ok(out)
    }
}

/// Unix milliseconds, used for modification timestamps.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Sent by a player entering a peer's zone: "I am now in your chunk, send me
/// the live state."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneJoinRequest {
    /// The requesting player's peer id.
    pub peer_id: String,
    /// The chunk (zone) being entered.
    pub chunk_x: i32,
    pub chunk_z: i32,
}

impl ZoneJoinRequest {
    /// Serializes to a wire datagram: `[ZONE_JOIN_TAG] + JSON`.
    pub fn wire_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(48);
        out.push(ZONE_JOIN_TAG);
        out.extend_from_slice(&serde_json::to_vec(self).unwrap_or_default());
        out
    }

    /// Parses a wire datagram into a zone-join request, if it carries one.
    pub fn from_wire(bytes: &[u8]) -> Option<Self> {
        if bytes.first() != Some(&ZONE_JOIN_TAG) {
            return None;
        }
        serde_json::from_slice(&bytes[1..]).ok()
    }
}

/// A zone owner's live chunk state, sent in reply to a [`ZoneJoinRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoneState {
    /// The zone owner's peer id.
    pub owner: String,
    /// The chunk this state describes.
    pub chunk: ChunkCoord,
    /// The live state (edits) of the chunk.
    pub state: ChunkState,
}

impl ZoneState {
    /// Serializes to a wire datagram: `[ZONE_STATE_TAG] + JSON`.
    pub fn wire_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256);
        out.push(ZONE_STATE_TAG);
        out.extend_from_slice(&serde_json::to_vec(self).unwrap_or_default());
        out
    }

    /// Parses a wire datagram into a zone state, if it carries one.
    pub fn from_wire(bytes: &[u8]) -> Option<Self> {
        if bytes.first() != Some(&ZONE_STATE_TAG) {
            return None;
        }
        serde_json::from_slice(&bytes[1..]).ok()
    }
}
