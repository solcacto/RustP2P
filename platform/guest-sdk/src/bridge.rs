//! The raw host bridge.
//!
//! On `wasm32` these call the host's `env` host functions directly (the exact
//! set the RustP2P host registers). On native they read/write an in-memory
//! [`MockState`] so the SDK can be unit-tested without a wasm runtime.

/// A 16-byte pose layout matching the host wire format.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct Pose {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub rot_y: f32,
}

impl Pose {
    pub const fn zero() -> Self {
        Self { x: 0.0, y: 0.0, z: 0.0, rot_y: 0.0 }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::Pose;

    /// Raw host imports. Names must match the host's `env` registrations.
    mod raw {
        use crate::bridge::Pose;

        #[link(wasm_import_module = "env")]
        extern "C" {
            pub fn get_frame_count() -> u64;
            pub fn get_delta_seconds() -> f64;
            pub fn draw_box(x: f32, y: f32, z: f32, sx: f32, sy: f32, sz: f32, r: f32, g: f32, b: f32);
            pub fn get_input_move_up() -> u32;
            pub fn get_input_move_down() -> u32;
            pub fn get_input_move_left() -> u32;
            pub fn get_input_move_right() -> u32;
            pub fn get_input_action_1() -> u32;
            pub fn get_input_action_2() -> u32;
            pub fn get_input_gamepad_connected() -> u32;
            pub fn get_input_gamepad_axis_x() -> f32;
            pub fn get_input_gamepad_axis_y() -> f32;
            pub fn send_network_message(peer_id_ptr: *const u8, peer_id_len: usize, msg_ptr: *const u8, msg_len: usize) -> i32;
            pub fn receive_network_message(buffer_ptr: *mut u8, buffer_len: usize) -> i32;
            pub fn update_avatar_transform(x: f32, y: f32, z: f32, rot_y: f32);
            pub fn broadcast_avatar_pose(x: f32, y: f32, z: f32, rot_y: f32);
            pub fn get_remote_avatar_pose(peer_id_ptr: *const u8, peer_id_len: usize, out_pose_ptr: *mut Pose) -> i32;
            pub fn get_movement_axis() -> i32;
            pub fn set_tag_score(score: u32);
            pub fn broadcast_tag_score(score: u32);
            pub fn load_avatar(path_ptr: *const u8, path_len: usize) -> i32;
            pub fn broadcast_chunk_claim(origin_x: i32, origin_z: i32, extent_x: i32, extent_z: i32);
            pub fn get_chunk_owner(chunk_x: i32, chunk_z: i32, out_peer_ptr: *mut u8, out_peer_len: usize) -> i32;
            pub fn send_reliable_message(peer_id_ptr: *const u8, peer_id_len: usize, msg_ptr: *const u8, msg_len: usize) -> i32;
            pub fn receive_reliable_message(buffer_ptr: *mut u8, buffer_len: usize) -> i32;
            pub fn save_chunk_state(edit_x: f32, edit_z: f32, kind_ptr: *const u8, kind_len: usize, value: f32) -> i32;
            pub fn publish_chunk_states() -> i32;
            pub fn load_remote_chunk_state(chunk_x: i32, chunk_z: i32, out_buf: *mut u8, out_len: usize) -> i32;
        }
    }

    pub fn frame_count() -> u64 { unsafe { raw::get_frame_count() } }
    pub fn delta_seconds() -> f64 { unsafe { raw::get_delta_seconds() } }
    pub fn draw_box(x: f32, y: f32, z: f32, sx: f32, sy: f32, sz: f32, r: f32, g: f32, b: f32) {
        unsafe { raw::draw_box(x, y, z, sx, sy, sz, r, g, b) };
    }
    pub fn input_move_up() -> bool { unsafe { raw::get_input_move_up() != 0 } }
    pub fn input_move_down() -> bool { unsafe { raw::get_input_move_down() != 0 } }
    pub fn input_move_left() -> bool { unsafe { raw::get_input_move_left() != 0 } }
    pub fn input_move_right() -> bool { unsafe { raw::get_input_move_right() != 0 } }
    pub fn input_action_1() -> bool { unsafe { raw::get_input_action_1() != 0 } }
    pub fn input_action_2() -> bool { unsafe { raw::get_input_action_2() != 0 } }
    pub fn input_gamepad_connected() -> bool { unsafe { raw::get_input_gamepad_connected() != 0 } }
    pub fn input_gamepad_axis_x() -> f32 { unsafe { raw::get_input_gamepad_axis_x() } }
    pub fn input_gamepad_axis_y() -> f32 { unsafe { raw::get_input_gamepad_axis_y() } }
    pub fn movement_axis() -> u32 { unsafe { raw::get_movement_axis() as u32 } }

    pub fn send_network_message(peer_id: &[u8], msg: &[u8]) -> bool {
        unsafe {
            raw::send_network_message(peer_id.as_ptr(), peer_id.len(), msg.as_ptr(), msg.len()) != 0
        }
    }

    pub fn receive_network_message(buffer: &mut [u8]) -> Option<usize> {
        if buffer.is_empty() {
            return None;
        }
        let n = unsafe { raw::receive_network_message(buffer.as_mut_ptr(), buffer.len()) };
        if n < 0 {
            None
        } else {
            Some(n as usize)
        }
    }

    pub fn update_avatar_transform(x: f32, y: f32, z: f32, rot_y: f32) {
        unsafe { raw::update_avatar_transform(x, y, z, rot_y) };
    }

    pub fn broadcast_avatar_pose(x: f32, y: f32, z: f32, rot_y: f32) {
        unsafe { raw::broadcast_avatar_pose(x, y, z, rot_y) };
    }

    pub fn get_remote_avatar_pose(peer_id: &[u8]) -> Option<Pose> {
        let mut pose = Pose::zero();
        let found = unsafe {
            raw::get_remote_avatar_pose(peer_id.as_ptr(), peer_id.len(), &mut pose)
        };
        if found != 0 { Some(pose) } else { None }
    }

    pub fn set_tag_score(score: u32) {
        unsafe { raw::set_tag_score(score) };
    }

    pub fn broadcast_tag_score(score: u32) {
        unsafe { raw::broadcast_tag_score(score) };
    }

    pub fn load_avatar(path: &[u8]) -> bool {
        unsafe { raw::load_avatar(path.as_ptr(), path.len()) != 0 }
    }

    pub fn broadcast_chunk_claim(origin_x: i32, origin_z: i32, extent_x: i32, extent_z: i32) {
        unsafe { raw::broadcast_chunk_claim(origin_x, origin_z, extent_x, extent_z) };
    }

    pub fn get_chunk_owner(chunk_x: i32, chunk_z: i32, buffer: &mut [u8]) -> Option<usize> {
        if buffer.is_empty() {
            return None;
        }
        let n = unsafe { raw::get_chunk_owner(chunk_x, chunk_z, buffer.as_mut_ptr(), buffer.len()) };
        if n != 0 { Some(n as usize) } else { None }
    }

    pub fn send_reliable_message(peer_id: &[u8], msg: &[u8]) -> bool {
        unsafe {
            raw::send_reliable_message(peer_id.as_ptr(), peer_id.len(), msg.as_ptr(), msg.len()) != 0
        }
    }

    pub fn receive_reliable_message(buffer: &mut [u8]) -> Option<usize> {
        if buffer.is_empty() {
            return None;
        }
        let n = unsafe { raw::receive_reliable_message(buffer.as_mut_ptr(), buffer.len()) };
        if n <= 0 { None } else { Some(n as usize) }
    }

    pub fn save_chunk_state(edit_x: f32, edit_z: f32, kind: &[u8], value: f32) -> bool {
        unsafe { raw::save_chunk_state(edit_x, edit_z, kind.as_ptr(), kind.len(), value) != 0 }
    }

    pub fn publish_chunk_states() -> u32 {
        unsafe { raw::publish_chunk_states() as u32 }
    }

    pub fn load_remote_chunk_state(chunk_x: i32, chunk_z: i32, buffer: &mut [u8]) -> Option<usize> {
        if buffer.is_empty() {
            return None;
        }
        let n = unsafe { raw::load_remote_chunk_state(chunk_x, chunk_z, buffer.as_mut_ptr(), buffer.len()) };
        if n != 0 { Some(n as usize) } else { None }
    }
}

/// Native test double: records what the game asked for and feeds it back.
#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::Pose;
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    /// Shared mock state driven by `guest_sdk` unit tests.
    #[derive(Default)]
    pub struct MockState {
        pub frame_count: u64,
        pub delta_seconds: f64,
        pub draw_boxes: Vec<[f32; 9]>,
        pub move_up: bool,
        pub move_down: bool,
        pub move_left: bool,
        pub move_right: bool,
        pub action_1: bool,
        pub action_2: bool,
        pub gamepad_connected: bool,
        pub gamepad_axis_x: f32,
        pub gamepad_axis_y: f32,
        pub movement_axis: u32,
        pub avatar: [f32; 4],
        pub last_pose: Option<[f32; 4]>,
        pub remote_pose: Option<Pose>,
        pub last_score: Option<u32>,
        pub avatar_path: Option<String>,
        pub sent_messages: Vec<(String, Vec<u8>)>,
        pub received_messages: Vec<Vec<u8>>,
        pub chunk_claims: Vec<(i32, i32, i32, i32)>,
        pub chunk_owners: HashMap<(i32, i32), String>,
        pub reliable_sent: Vec<(String, Vec<u8>)>,
        pub reliable_received: Vec<Vec<u8>>,
        pub saved_edits: Vec<(f32, f32, String, f32)>,
        pub published_states: u32,
        pub remote_state: Option<String>,
    }

    static MOCK: LazyLock<Mutex<MockState>> = LazyLock::new(|| Mutex::new(MockState::default()));

    /// Grants exclusive access to the mock state (for tests).
    pub fn mock() -> std::sync::MutexGuard<'static, MockState> {
        MOCK.lock().unwrap()
    }

    pub fn frame_count() -> u64 { MOCK.lock().unwrap().frame_count }

    pub fn delta_seconds() -> f64 {
        let s = MOCK.lock().unwrap();
        if s.delta_seconds == 0.0 {
            1.0 / 60.0
        } else {
            s.delta_seconds
        }
    }

    pub fn draw_box(x: f32, y: f32, z: f32, sx: f32, sy: f32, sz: f32, r: f32, g: f32, b: f32) {
        MOCK.lock().unwrap().draw_boxes.push([x, y, z, sx, sy, sz, r, g, b]);
    }
    pub fn input_move_up() -> bool { MOCK.lock().unwrap().move_up }
    pub fn input_move_down() -> bool { MOCK.lock().unwrap().move_down }
    pub fn input_move_left() -> bool { MOCK.lock().unwrap().move_left }
    pub fn input_move_right() -> bool { MOCK.lock().unwrap().move_right }
    pub fn input_action_1() -> bool { MOCK.lock().unwrap().action_1 }
    pub fn input_action_2() -> bool { MOCK.lock().unwrap().action_2 }
    pub fn input_gamepad_connected() -> bool { MOCK.lock().unwrap().gamepad_connected }
    pub fn input_gamepad_axis_x() -> f32 { MOCK.lock().unwrap().gamepad_axis_x }
    pub fn input_gamepad_axis_y() -> f32 { MOCK.lock().unwrap().gamepad_axis_y }
    pub fn movement_axis() -> u32 { MOCK.lock().unwrap().movement_axis }

    pub fn send_network_message(peer_id: &[u8], msg: &[u8]) -> bool {
        let mut s = MOCK.lock().unwrap();
        s.sent_messages.push((
            String::from_utf8_lossy(peer_id).into_owned(),
            msg.to_vec(),
        ));
        true
    }

    pub fn receive_network_message(buffer: &mut [u8]) -> Option<usize> {
        let mut s = MOCK.lock().unwrap();
        let msg = s.received_messages.pop()?;
        let n = msg.len().min(buffer.len());
        buffer[..n].copy_from_slice(&msg[..n]);
        Some(n)
    }

    pub fn update_avatar_transform(x: f32, y: f32, z: f32, rot_y: f32) {
        MOCK.lock().unwrap().avatar = [x, y, z, rot_y];
    }

    pub fn broadcast_avatar_pose(x: f32, y: f32, z: f32, rot_y: f32) {
        MOCK.lock().unwrap().last_pose = Some([x, y, z, rot_y]);
    }

    pub fn get_remote_avatar_pose(peer_id: &[u8]) -> Option<Pose> {
        let s = MOCK.lock().unwrap();
        if s.remote_pose.is_some() && !peer_id.is_empty() {
            s.remote_pose
        } else {
            None
        }
    }

    pub fn set_tag_score(score: u32) {
        MOCK.lock().unwrap().last_score = Some(score);
    }

    pub fn broadcast_tag_score(score: u32) {
        MOCK.lock().unwrap().last_score = Some(score);
    }

    pub fn load_avatar(path: &[u8]) -> bool {
        let path = String::from_utf8_lossy(path).into_owned();
        MOCK.lock().unwrap().avatar_path = Some(path);
        true
    }

    pub fn broadcast_chunk_claim(origin_x: i32, origin_z: i32, extent_x: i32, extent_z: i32) {
        let mut s = MOCK.lock().unwrap();
        s.chunk_claims.push((origin_x, origin_z, extent_x, extent_z));
    }

    pub fn get_chunk_owner(chunk_x: i32, chunk_z: i32, buffer: &mut [u8]) -> Option<usize> {
        let s = MOCK.lock().unwrap();
        let owner = s.chunk_owners.get(&(chunk_x, chunk_z))?;
        let bytes = owner.as_bytes();
        let n = bytes.len().min(buffer.len());
        buffer[..n].copy_from_slice(&bytes[..n]);
        Some(n)
    }

    pub fn send_reliable_message(peer_id: &[u8], msg: &[u8]) -> bool {
        let peer = String::from_utf8_lossy(peer_id).into_owned();
        MOCK.lock().unwrap().reliable_sent.push((peer, msg.to_vec()));
        true
    }

    pub fn receive_reliable_message(buffer: &mut [u8]) -> Option<usize> {
        let mut s = MOCK.lock().unwrap();
        let msg = s.reliable_received.pop()?;
        let n = msg.len().min(buffer.len());
        buffer[..n].copy_from_slice(&msg[..n]);
        Some(n)
    }

    pub fn save_chunk_state(edit_x: f32, edit_z: f32, kind: &[u8], value: f32) -> bool {
        let kind = String::from_utf8_lossy(kind).into_owned();
        MOCK.lock().unwrap().saved_edits.push((edit_x, edit_z, kind, value));
        true
    }

    pub fn publish_chunk_states() -> u32 {
        let mut s = MOCK.lock().unwrap();
        s.published_states += 1;
        s.published_states
    }

    pub fn load_remote_chunk_state(_chunk_x: i32, _chunk_z: i32, buffer: &mut [u8]) -> Option<usize> {
        let s = MOCK.lock().unwrap();
        let json = s.remote_state.as_ref()?;
        let bytes = json.as_bytes();
        let n = bytes.len().min(buffer.len());
        buffer[..n].copy_from_slice(&bytes[..n]);
        Some(n)
    }
}

pub use imp::*;
