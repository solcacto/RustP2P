//! Per-instance host state shared with a running Wasm guest.
//!
//! Each `Store<HostState>` owns exactly one [`HostState`], so no state can
//! leak between guest modules or instances. Host functions read and mutate it
//! through `Caller::data()` / `Caller::data_mut()`.

use crate::avatar_state::{AvatarPose, AvatarState};
use crate::chunk::{ChunkClaim, ChunkCoord, ChunkDht, ChunkEdit, ChunkState, ChunkStatePointer, ChunkStore};
use crate::input_state::InputState;
use crate::ipfs::IpfsClient;
use crate::peer_connection::PeerConnection;
use anyhow::Result;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Connectivity state of the signaling server link, surfaced in the HUD.
///
/// Defaults to [`SignalingStatus::Connected`]: the link was established
/// successfully at startup, so the health monitor starts optimistic and
/// corrects the status if a probe fails.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SignalingStatus {
    /// The WebSocket to the signaling server is healthy.
    #[default]
    Connected,
    /// A reconnect is in progress; `attempt` counts retries so far.
    Reconnecting { attempt: u32 },
    /// The link is known to be unavailable.
    Down,
}

impl SignalingStatus {
    /// Human-readable label for HUD/log output.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Reconnecting { .. } => "reconnecting",
            Self::Down => "down",
        }
    }

    /// Returns the current retry attempt number (`0` when connected/down).
    pub fn attempt(&self) -> u32 {
        match self {
            Self::Reconnecting { attempt } => *attempt,
            _ => 0,
        }
    }
}

/// All state the host keeps for a single Wasm instance.
///
/// This is the only handle a guest has to the host's world: host functions
/// read the fields below (input, peer connection, avatar state) and mutate the
/// ones the guest is allowed to write (score, poses). Everything is owned here
/// rather than in globals, keeping instances isolated from one another.
pub struct HostState {
    counter: u32,
    name: String,
    frame_count: u64,
    last_frame_time: Instant,
    delta_time: f64,
    input_state: InputState,
    previous_input_state: InputState,
    peer_id: String,
    peer_connection: Option<PeerConnection>,
    connected_peers: Vec<String>,
    incoming_messages: VecDeque<Vec<u8>>,
    avatar_state: Option<Arc<Mutex<AvatarState>>>,
    /// Latest pose of every remote peer's avatar, shared with the renderer.
    remote_avatars: Arc<Mutex<HashMap<String, AvatarPose>>>,
    /// Latest tag score of every remote peer, shared with the renderer.
    remote_scores: Arc<Mutex<HashMap<String, u32>>>,
    /// Local movement axis: 0 = X (role A), 1 = Z (role B).
    movement_axis: u8,
    /// Local tag score reported by the guest through `set_tag_score`.
    tag_score: u32,
    /// Latest Wasm guest error (trap), if any. Set instead of crashing.
    wasm_error: Option<String>,
    /// Frame at which `wasm_error` was set; used to exit gracefully after a
    /// grace period.
    wasm_error_frame: Option<u64>,
    /// Signaling server link health, updated by the health monitor.
    signaling_status: SignalingStatus,
    /// Last frame the signaling link was probed.
    last_signaling_probe: Option<u64>,
    /// Earliest `Instant` a reconnect may be attempted (exponential backoff).
    signaling_next_retry: Option<Instant>,
    /// Avatar asset path (relative to the host asset folder) currently in use.
    avatar_path: String,
    /// Shared kad-style DHT mapping chunk coordinates to hosting peers.
    chunk_registry: Arc<Mutex<ChunkDht>>,
    /// The local peer's own chunk claims (what it hosts for the world).
    owned_chunks: Vec<ChunkCoord>,
    /// In-memory chunk states this peer owns (persisted locally + to IPFS).
    chunk_states: HashMap<ChunkCoord, ChunkState>,
    /// The peer's identity key (`ed25519:<hex>`), part of the IPFS cache key.
    owner_pubkey: String,
    /// IPFS node API used for publishing/fetching chunk state.
    ipfs_api: String,
}

impl HostState {
    /// Creates a fresh instance whose peer identity is `name`.
    ///
    /// The same name is reported by [`HostState::peer_id`] and used as the
    /// identity this node registers with the signaling server.
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            counter: 0,
            peer_id: name.clone(),
            name,
            frame_count: 0,
            last_frame_time: Instant::now(),
            delta_time: 0.0,
            input_state: InputState::default(),
            previous_input_state: InputState::default(),
            peer_connection: None,
            connected_peers: Vec::new(),
            incoming_messages: VecDeque::new(),
            avatar_state: None,
            remote_avatars: Arc::new(Mutex::new(HashMap::new())),
            remote_scores: Arc::new(Mutex::new(HashMap::new())),
            movement_axis: 0,
            tag_score: 0,
            wasm_error: None,
            wasm_error_frame: None,
            signaling_status: SignalingStatus::default(),
            last_signaling_probe: None,
            signaling_next_retry: None,
            avatar_path: "avatar_standard.glb".to_string(),
            chunk_registry: Arc::new(Mutex::new(ChunkDht::new())),
            owned_chunks: Vec::new(),
            chunk_states: HashMap::new(),
            owner_pubkey: String::new(),
            ipfs_api: crate::ipfs::IPFS_API.to_string(),
        }
    }

    /// Increments the demo counter exposed through the `increment_counter`
    /// host function.
    pub fn increment_counter(&mut self) {
        self.counter += 1;
    }

    /// Returns the current value of the demo counter.
    pub fn counter(&self) -> u32 {
        self.counter
    }

    /// Returns the instance name (identical to the peer id).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Advances the frame clock and returns the delta time since the previous
    /// call, in seconds.
    ///
    /// Called once per rendered frame by the renderer before the guest tick.
    pub fn update_frame(&mut self) -> f64 {
        let now = Instant::now();
        let delta = now.duration_since(self.last_frame_time).as_secs_f64();
        self.delta_time = delta;
        self.frame_count += 1;
        self.last_frame_time = now;
        delta
    }

    /// Returns the number of frames rendered so far.
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Returns the delta time of the most recent frame, in seconds.
    pub fn delta_time(&self) -> f64 {
        self.delta_time
    }

    /// Sets the current frame's input, rotating the previous frame's input.
    pub fn set_input(&mut self, input: InputState) {
        self.previous_input_state = self.input_state;
        self.input_state = input;
    }

    /// Returns the current frame's input state.
    pub fn input(&self) -> &InputState {
        &self.input_state
    }

    /// Returns the previous frame's input state.
    pub fn previous_input(&self) -> &InputState {
        &self.previous_input_state
    }

    /// Returns this node's peer id, as registered with the signaling server.
    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    /// Replaces the peer connection, or removes it when passed `None`.
    pub fn set_peer_connection(&mut self, pc: Option<PeerConnection>) {
        self.peer_connection = pc;
    }

    /// Returns the live peer connection, if one has been established.
    pub fn peer_connection(&self) -> Option<&PeerConnection> {
        self.peer_connection.as_ref()
    }

    /// Returns the live peer connection mutably, if one has been established.
    pub fn peer_connection_mut(&mut self) -> Option<&mut PeerConnection> {
        self.peer_connection.as_mut()
    }

    /// Returns the ids of peers this node has established links with.
    pub fn connected_peers(&self) -> &[String] {
        &self.connected_peers
    }

    /// Returns the mutable list of connected peer ids.
    pub fn connected_peers_mut(&mut self) -> &mut Vec<String> {
        &mut self.connected_peers
    }

    /// Returns the queue of inbound network messages for the guest.
    pub fn incoming_messages(&self) -> &VecDeque<Vec<u8>> {
        &self.incoming_messages
    }

    /// Returns the mutable queue of inbound network messages for the guest.
    pub fn incoming_messages_mut(&mut self) -> &mut VecDeque<Vec<u8>> {
        &mut self.incoming_messages
    }

    /// Attaches the shared local avatar pose written by the guest, or detaches
    /// it when passed `None`.
    pub fn set_avatar_state(&mut self, state: Option<Arc<Mutex<AvatarState>>>) {
        self.avatar_state = state;
    }

    /// Returns the shared local avatar pose handle, if attached.
    pub fn avatar_state(&self) -> Option<&Arc<Mutex<AvatarState>>> {
        self.avatar_state.as_ref()
    }

    /// Replaces the shared map of remote avatar poses (also handed to the
    /// renderer so both sides observe the same data).
    pub fn set_remote_avatars(&mut self, map: Arc<Mutex<HashMap<String, AvatarPose>>>) {
        self.remote_avatars = map;
    }

    /// Returns the shared map of the latest pose of every remote peer.
    pub fn remote_avatars(&self) -> &Arc<Mutex<HashMap<String, AvatarPose>>> {
        &self.remote_avatars
    }

    /// Sets which movement axis this role uses (0 = X, 1 = Z).
    pub fn set_movement_axis(&mut self, axis: u8) {
        self.movement_axis = axis;
    }

    /// Returns the movement axis for this role.
    pub fn movement_axis(&self) -> u8 {
        self.movement_axis
    }

    /// Records the local tag score reported by the guest.
    pub fn set_tag_score(&mut self, score: u32) {
        self.tag_score = score;
    }

    /// Returns the latest local tag score reported by the guest.
    pub fn tag_score(&self) -> u32 {
        self.tag_score
    }

    /// Replaces the shared map of remote peers' tag scores.
    pub fn set_remote_scores(&mut self, map: Arc<Mutex<HashMap<String, u32>>>) {
        self.remote_scores = map;
    }

    /// Returns the shared map of the latest tag score of every remote peer.
    pub fn remote_scores(&self) -> &Arc<Mutex<HashMap<String, u32>>> {
        &self.remote_scores
    }

    /// Records a Wasm guest trap, along with the frame it happened on.
    ///
    /// The host treats this as a recoverable condition: it stops calling the
    /// guest, surfaces the error in the HUD, and exits gracefully after a
    /// grace period instead of crashing.
    pub fn set_wasm_error(&mut self, error: String, frame: u64) {
        self.wasm_error = Some(error);
        self.wasm_error_frame = Some(frame);
    }

    /// Returns the latest Wasm guest error, if the guest has trapped.
    pub fn wasm_error(&self) -> Option<&str> {
        self.wasm_error.as_deref()
    }

    /// Returns the frame on which the guest trapped, if it did.
    pub fn wasm_error_frame(&self) -> Option<u64> {
        self.wasm_error_frame
    }

    /// Replaces the signaling link health status.
    pub fn set_signaling_status(&mut self, status: SignalingStatus) {
        self.signaling_status = status;
    }

    /// Returns the signaling link health status.
    pub fn signaling_status(&self) -> &SignalingStatus {
        &self.signaling_status
    }

    /// Records the last frame the signaling link was probed.
    pub fn set_last_signaling_probe(&mut self, frame: Option<u64>) {
        self.last_signaling_probe = frame;
    }

    /// Returns the last frame the signaling link was probed.
    pub fn last_signaling_probe(&self) -> Option<u64> {
        self.last_signaling_probe
    }

    /// Sets the earliest time a reconnect may be attempted.
    pub fn set_signaling_next_retry(&mut self, when: Option<Instant>) {
        self.signaling_next_retry = when;
    }

    /// Returns the earliest time a reconnect may be attempted.
    pub fn signaling_next_retry(&self) -> Option<Instant> {
        self.signaling_next_retry
    }

    /// Sets the avatar asset path (relative to the host asset folder).
    pub fn set_avatar_path(&mut self, path: impl Into<String>) {
        self.avatar_path = path.into();
    }

    /// Returns the avatar asset path currently in use.
    pub fn avatar_path(&self) -> &str {
        &self.avatar_path
    }

    /// Returns the shared chunk DHT (chunk coordinate → hosting peer).
    pub fn chunk_registry(&self) -> &Arc<Mutex<ChunkDht>> {
        &self.chunk_registry
    }

    /// Replaces the shared chunk DHT (used by tests and the renderer).
    pub fn set_chunk_registry(&mut self, dht: Arc<Mutex<ChunkDht>>) {
        self.chunk_registry = dht;
    }

    /// The chunk coordinates this peer currently hosts.
    pub fn owned_chunks(&self) -> &[ChunkCoord] {
        &self.owned_chunks
    }

    /// Claims a rectangular region of chunks on behalf of the local peer:
    /// records the ownership in the local DHT and broadcasts the claim to
    /// every known peer over the P2P mesh.
    pub fn claim_chunk_region(&mut self, origin: ChunkCoord, extent: (u32, u32)) -> Result<()> {
        let claim = ChunkClaim::region(self.peer_id.clone(), origin, extent);
        self.owned_chunks = claim.coords();
        let address = match self.peer_connection() {
            Some(pc) => pc.local_addr()?,
            None => "127.0.0.1:0".parse().expect("valid placeholder address"),
        };
        self.chunk_registry.lock().unwrap().apply_claim(&claim, address);
        if let Some(pc) = self.peer_connection() {
            pc.send_to_all(&claim.wire_bytes())?;
        }
        Ok(())
    }

    /// Claims the chunk containing `(x, z)` plus the adjacent chunk to +X
    /// (the classic "I am hosting chunks X,Y and X+1,Y" claim).
    pub fn claim_around_position(&mut self, x: f32, z: f32) -> Result<()> {
        let origin = ChunkCoord::at(x, z);
        self.claim_chunk_region(origin, (2, 1))
    }

    /// Sets this peer's identity key (`ed25519:<hex>`).
    pub fn set_owner_pubkey(&mut self, pubkey: impl Into<String>) {
        self.owner_pubkey = pubkey.into();
    }

    /// This peer's identity key.
    pub fn owner_pubkey(&self) -> &str {
        &self.owner_pubkey
    }

    /// Sets the IPFS node API used for chunk state (defaults to the local node).
    pub fn set_ipfs_api(&mut self, api: impl Into<String>) {
        self.ipfs_api = api.into();
    }

    /// The IPFS node API this host uses.
    pub fn ipfs_api(&self) -> &str {
        &self.ipfs_api
    }

    /// Records a chunk modification: the owning peer saves it locally (JSON)
    /// so it can be re-published later.
    pub fn record_chunk_edit(&mut self, edit: ChunkEdit) -> Result<()> {
        let coord = ChunkCoord::at(edit.x, edit.z);
        let state = self.chunk_states.entry(coord).or_insert_with(|| ChunkState {
            chunk: coord,
            owner: self.peer_id.clone(),
            owner_pubkey: self.owner_pubkey.clone(),
            modified_at: crate::chunk::now_ms(),
            edits: Vec::new(),
        });
        state.apply(edit);
        ChunkStore::new(&self.peer_id).save(state)
    }

    /// The chunk states this peer currently owns in memory.
    pub fn chunk_states(&self) -> &HashMap<ChunkCoord, ChunkState> {
        &self.chunk_states
    }

    /// Publishes every locally-saved chunk state to IPFS, records the CIDs in
    /// the chunk DHT (keyed by chunk coordinate + owner key), and broadcasts a
    /// state pointer to every peer so they can cache the "ruins" once the
    /// owner goes offline. Returns how many states were published.
    pub fn publish_chunk_states(&mut self) -> Result<usize> {
        let ipfs = IpfsClient::new(self.ipfs_api.clone());
        let states: Vec<ChunkState> = self.chunk_states.values().cloned().collect();
        let mut published = 0;
        for state in &states {
            let json = serde_json::to_vec(state)?;
            let cid = ipfs.add_bytes(&json)?;
            let address = match self.peer_connection() {
                Some(pc) => pc.local_addr()?,
                None => "127.0.0.1:0".parse().expect("valid placeholder address"),
            };
            self.chunk_registry.lock().unwrap().record_state(
                state.chunk,
                cid.clone(),
                state.owner_pubkey.clone(),
                state.owner.clone(),
                address,
            );
            let pointer = ChunkStatePointer {
                peer_id: state.owner.clone(),
                owner_pubkey: state.owner_pubkey.clone(),
                chunk_x: state.chunk.x,
                chunk_z: state.chunk.z,
                state_cid: cid.clone(),
            };
            if let Some(pc) = self.peer_connection() {
                pc.send_to_all(&pointer.wire_bytes())?;
            }
            println!(
                "[chunk-state] published chunk ({},{}) -> {cid} (key: {})",
                state.chunk.x,
                state.chunk.z,
                pointer.key()
            );
            published += 1;
        }
        Ok(published)
    }

    /// Loads a remote chunk's persisted state from IPFS by the CID recorded in
    /// the chunk DHT — the "ruins" cache, available while the owner is offline.
    pub fn load_remote_chunk_state(&self, coord: ChunkCoord) -> Result<Option<ChunkState>> {
        let entry = self.chunk_registry.lock().unwrap().get(coord).cloned();
        let Some(cid) = entry.and_then(|e| e.state_cid) else {
            return Ok(None);
        };
        let bytes = IpfsClient::new(self.ipfs_api.clone()).cat(&cid)?;
        Ok(serde_json::from_slice(&bytes).ok())
    }

    /// Records a remote chunk-state pointer (received over the mesh) in the
    /// DHT and warms the local IPFS cache so the state survives the owner's
    /// departure.
    pub fn ingest_state_pointer(&mut self, pointer: &ChunkStatePointer, addr: std::net::SocketAddr) {
        self.chunk_registry.lock().unwrap().record_state(
            ChunkCoord { x: pointer.chunk_x, z: pointer.chunk_z },
            pointer.state_cid.clone(),
            pointer.owner_pubkey.clone(),
            pointer.peer_id.clone(),
            addr,
        );
        // Warm the cache: fetch + pin now, while the owner may still be online.
        if let Ok(bytes) = IpfsClient::new(self.ipfs_api.clone()).cat(&pointer.state_cid) {
            let _ = serde_json::from_slice::<ChunkState>(&bytes);
        }
    }
}