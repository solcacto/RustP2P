use crate::avatar_state::{AvatarPose, AvatarState};
use crate::input_state::InputState;
use crate::peer_connection::PeerConnection;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
}

impl HostState {
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
        }
    }

    pub fn increment_counter(&mut self) {
        self.counter += 1;
    }

    pub fn counter(&self) -> u32 {
        self.counter
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn update_frame(&mut self) -> f64 {
        let now = Instant::now();
        let delta = now.duration_since(self.last_frame_time).as_secs_f64();
        self.delta_time = delta;
        self.frame_count += 1;
        self.last_frame_time = now;
        delta
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    pub fn delta_time(&self) -> f64 {
        self.delta_time
    }

    /// Sets the current frame's input, rotating the previous frame's input.
    pub fn set_input(&mut self, input: InputState) {
        self.previous_input_state = self.input_state;
        self.input_state = input;
    }

    pub fn input(&self) -> &InputState {
        &self.input_state
    }

    pub fn previous_input(&self) -> &InputState {
        &self.previous_input_state
    }

    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    pub fn set_peer_connection(&mut self, pc: Option<PeerConnection>) {
        self.peer_connection = pc;
    }

    pub fn peer_connection(&self) -> Option<&PeerConnection> {
        self.peer_connection.as_ref()
    }

    pub fn peer_connection_mut(&mut self) -> Option<&mut PeerConnection> {
        self.peer_connection.as_mut()
    }

    pub fn connected_peers(&self) -> &[String] {
        &self.connected_peers
    }

    pub fn connected_peers_mut(&mut self) -> &mut Vec<String> {
        &mut self.connected_peers
    }

    pub fn incoming_messages(&self) -> &VecDeque<Vec<u8>> {
        &self.incoming_messages
    }

    pub fn incoming_messages_mut(&mut self) -> &mut VecDeque<Vec<u8>> {
        &mut self.incoming_messages
    }

    pub fn set_avatar_state(&mut self, state: Option<Arc<Mutex<AvatarState>>>) {
        self.avatar_state = state;
    }

    pub fn avatar_state(&self) -> Option<&Arc<Mutex<AvatarState>>> {
        self.avatar_state.as_ref()
    }

    pub fn set_remote_avatars(&mut self, map: Arc<Mutex<HashMap<String, AvatarPose>>>) {
        self.remote_avatars = map;
    }

    pub fn remote_avatars(&self) -> &Arc<Mutex<HashMap<String, AvatarPose>>> {
        &self.remote_avatars
    }

    pub fn set_movement_axis(&mut self, axis: u8) {
        self.movement_axis = axis;
    }

    pub fn movement_axis(&self) -> u8 {
        self.movement_axis
    }

    pub fn set_tag_score(&mut self, score: u32) {
        self.tag_score = score;
    }

    pub fn tag_score(&self) -> u32 {
        self.tag_score
    }

    pub fn set_remote_scores(&mut self, map: Arc<Mutex<HashMap<String, u32>>>) {
        self.remote_scores = map;
    }

    pub fn remote_scores(&self) -> &Arc<Mutex<HashMap<String, u32>>> {
        &self.remote_scores
    }
}