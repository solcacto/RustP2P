//! The RustP2P guest SDK.
//!
//! Wraps the raw `env` host functions in a safe, ergonomic API so game
//! developers never touch `extern "C"` or wasm memory. Games implement the
//! [`Game`] trait and expose their entry point with the `#[export]` attribute;
//! the SDK handles instance lifecycle and dispatch.
//!
//! ```no_run
//! use guest_sdk::prelude::*;
//!
//! struct MyGame { x: f32, z: f32 }
//!
//! impl Game for MyGame {
//!     fn new() -> Self { Self { x: 0.0, z: 0.0 } }
//!     fn tick(&mut self, ctx: &mut Context) {
//!         let input = ctx.input();
//!         if input.up { self.z -= 0.05; }
//!         ctx.set_avatar_transform(self.x, 0.0, self.z, 0.0);
//!         ctx.broadcast_pose(self.x, 0.0, self.z, 0.0);
//!     }
//! }
//!
//! #[export(MyGame)]
//! fn game_tick() {}
//! ```

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod bridge;

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Snapshot of the current frame's input, read from the host.
#[derive(Debug, Clone, Copy, Default)]
pub struct InputState {
    /// "Move up" held this frame.
    pub up: bool,
    /// "Move down" held this frame.
    pub down: bool,
    /// "Move left" held this frame.
    pub left: bool,
    /// "Move right" held this frame.
    pub right: bool,
    /// Primary action held this frame.
    pub action_1: bool,
    /// Secondary action held this frame.
    pub action_2: bool,
    /// Whether a gamepad is connected.
    pub gamepad_connected: bool,
    /// Gamepad stick X axis in `[-1, 1]`.
    pub gamepad_axis_x: f32,
    /// Gamepad stick Y axis in `[-1, 1]`.
    pub gamepad_axis_y: f32,
}

/// A 3D pose (`x`, `y`, `z`, yaw `rot_y`).
pub type Pose = bridge::Pose;

/// The game interface every guest implements.
///
/// `new()` is called once, lazily, on the first tick; `tick()` runs once per
/// rendered frame with a [`Context`] that exposes the host's capabilities.
pub trait Game {
    /// Creates the game instance (called once before the first tick).
    fn new() -> Self
    where
        Self: Sized;

    /// Advances the game one frame.
    fn tick(&mut self, ctx: &mut Context);
}

/// Safe access to the host's capabilities for one frame.
pub struct Context;

impl Context {
    /// Returns the current frame number.
    pub fn frame_count(&self) -> u64 {
        bridge::frame_count()
    }

    /// Reads this frame's input snapshot.
    pub fn input(&self) -> InputState {
        InputState {
            up: bridge::input_move_up(),
            down: bridge::input_move_down(),
            left: bridge::input_move_left(),
            right: bridge::input_move_right(),
            action_1: bridge::input_action_1(),
            action_2: bridge::input_action_2(),
            gamepad_connected: bridge::input_gamepad_connected(),
            gamepad_axis_x: bridge::input_gamepad_axis_x(),
            gamepad_axis_y: bridge::input_gamepad_axis_y(),
        }
    }

    /// Returns this role's movement axis (`0` = X, `1` = Z).
    pub fn movement_axis(&self) -> u32 {
        bridge::movement_axis()
    }

    /// Sets the local avatar's target pose.
    pub fn set_avatar_transform(&self, x: f32, y: f32, z: f32, rot_y: f32) {
        bridge::update_avatar_transform(x, y, z, rot_y);
    }

    /// Broadcasts the local pose to every known peer.
    pub fn broadcast_pose(&self, x: f32, y: f32, z: f32, rot_y: f32) {
        bridge::broadcast_avatar_pose(x, y, z, rot_y);
    }

    /// Returns the latest known pose of `peer_id`, if it has reported one.
    pub fn remote_pose(&self, peer_id: &str) -> Option<Pose> {
        bridge::get_remote_avatar_pose(peer_id.as_bytes())
    }

    /// Reports the local tag score to the host (shown in the HUD).
    pub fn set_tag_score(&self, score: u32) {
        bridge::set_tag_score(score);
    }

    /// Broadcasts the local tag score to every known peer.
    pub fn broadcast_tag_score(&self, score: u32) {
        bridge::broadcast_tag_score(score);
    }

    /// Sends a datagram to `peer_id`; returns true on success.
    pub fn send_message(&self, peer_id: &str, message: &[u8]) -> bool {
        bridge::send_network_message(peer_id.as_bytes(), message)
    }

    /// Sends a **reliable** message to `peer_id` (acked + retried by the host).
    pub fn send_reliable_message(&self, peer_id: &str, message: &[u8]) -> bool {
        bridge::send_reliable_message(peer_id.as_bytes(), message)
    }

    /// Receives the next reliably-delivered message into `buffer`, returning
    /// its length.
    pub fn receive_reliable_message(&self, buffer: &mut [u8]) -> Option<usize> {
        bridge::receive_reliable_message(buffer)
    }

    /// Receives the next inbound datagram into `buffer`, returning its length.
    pub fn receive_message(&self, buffer: &mut [u8]) -> Option<usize> {
        bridge::receive_network_message(buffer)
    }

    /// Selects the avatar asset (relative to the host asset folder).
    pub fn load_avatar(&self, path: &str) -> bool {
        bridge::load_avatar(path.as_bytes())
    }

    /// Claims a region of world chunks on behalf of this peer. The claim is
    /// recorded in the host's chunk DHT and broadcast to every known peer.
    pub fn claim_chunks(&self, origin_x: i32, origin_z: i32, extent_x: u32, extent_z: u32) {
        bridge::broadcast_chunk_claim(origin_x, origin_z, extent_x as i32, extent_z as i32);
    }

    /// Looks up who hosts a world chunk, returning their peer id if known.
    pub fn chunk_owner(&self, chunk_x: i32, chunk_z: i32) -> Option<String> {
        let mut buf = [0u8; 64];
        let n = bridge::get_chunk_owner(chunk_x, chunk_z, &mut buf)?;
        Some(String::from_utf8_lossy(&buf[..n]).into_owned())
    }

    /// Records a chunk modification for the chunk this peer owns (persisted
    /// locally and re-published to IPFS on shutdown).
    pub fn save_chunk_edit(&self, x: f32, z: f32, kind: &str, value: f32) -> bool {
        bridge::save_chunk_state(x, z, kind.as_bytes(), value)
    }

    /// Publishes every locally-saved chunk state to IPFS and broadcasts the
    /// state pointers so peers can cache them. Returns the number published.
    pub fn publish_chunk_states(&self) -> u32 {
        bridge::publish_chunk_states()
    }

    /// Loads a remote chunk's persisted state from IPFS (the "ruins"), even
    /// while its owner is offline, returning the state JSON.
    pub fn load_remote_chunk_state(&self, chunk_x: i32, chunk_z: i32) -> Option<String> {
        let mut buf = [0u8; 4096];
        let n = bridge::load_remote_chunk_state(chunk_x, chunk_z, &mut buf)?;
        Some(String::from_utf8_lossy(&buf[..n]).into_owned())
    }
}

/// Runs the game: lazily creates one instance per concrete `Game` type and
/// dispatches every tick through the SDK. This is what the `#[export]` macro
/// generates the C entry point for.
///
/// Instances are stored type-erased and keyed by `TypeId`, so each concrete
/// game type gets exactly one instance that survives across ticks.
pub fn run<G: Game + Send + 'static>() {
    static GAMES: OnceLock<Mutex<HashMap<TypeId, Box<dyn Any + Send>>>> = OnceLock::new();
    let games = GAMES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut games = games.lock().unwrap();
    let slot = games.entry(TypeId::of::<G>()).or_insert_with(|| Box::new(G::new()));
    let game = slot
        .downcast_mut::<G>()
        .expect("game instance was created for this exact type");
    let mut ctx = Context;
    game.tick(&mut ctx);
}

/// Convenience exports for `use guest_sdk::prelude::*;`.
pub mod prelude {
    pub use crate::{Context, Game, InputState, Pose, run};
    pub use guest_sdk_macros::export;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests: the SDK's per-type game instance and the shared mock
    /// bridge are global, so tests must not run concurrently.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// A trivial game used to drive the SDK through the native mock bridge.
    struct TestGame {
        x: f32,
        z: f32,
        ticks: u32,
    }

    impl Game for TestGame {
        fn new() -> Self {
            Self { x: 0.0, z: 0.0, ticks: 0 }
        }

        fn tick(&mut self, ctx: &mut Context) {
            self.ticks += 1;
            let input = ctx.input();
            if input.up {
                self.z -= 0.05;
            }
            if input.right {
                self.x += 0.1;
            }
            ctx.set_avatar_transform(self.x, 0.0, self.z, 0.0);
            ctx.broadcast_pose(self.x, 0.0, self.z, 0.0);
            if input.action_1 {
                ctx.set_tag_score(self.ticks);
            }
        }
    }

    /// A second, distinct game type so tests get their own per-type instance.
    struct TestGameB {
        x: f32,
        ticks: u32,
    }

    impl Game for TestGameB {
        fn new() -> Self {
            Self { x: 0.0, ticks: 0 }
        }

        fn tick(&mut self, ctx: &mut Context) {
            self.ticks += 1;
            if ctx.input().right {
                self.x += 0.1;
            }
            ctx.set_avatar_transform(self.x, 0.0, 0.0, 0.0);
        }
    }

    #[test]
    fn run_dispatches_ticks_and_drives_the_bridge() {
        let _test = TEST_LOCK.lock().unwrap();
        {
            let mut mock = crate::bridge::mock();
            mock.frame_count = 10;
            mock.move_up = true;
            mock.move_right = true;
            mock.action_1 = true;
            mock.remote_pose = Some(Pose { x: 3.0, y: 0.0, z: 0.0, rot_y: 0.0 });
        }

        run::<TestGame>();

        let mock = crate::bridge::mock();
        assert_eq!(mock.avatar[0], 0.1, "avatar x should reflect right input");
        assert_eq!(mock.avatar[2], -0.05, "avatar z should reflect up input");
        assert_eq!(mock.last_pose, Some([0.1, 0.0, -0.05, 0.0]), "pose broadcast");
        assert_eq!(mock.last_score, Some(1), "tag score reported");
        assert!(mock.remote_pose.is_some(), "remote pose readable");
    }

    #[test]
    fn run_keeps_one_instance() {
        let _test = TEST_LOCK.lock().unwrap();
        // Calling run twice must not re-create the game (state persists).
        {
            let mut mock = crate::bridge::mock();
            mock.move_right = true;
        }
        run::<TestGameB>();
        run::<TestGameB>();
        let mock = crate::bridge::mock();
        assert!(
            (mock.avatar[0] - 0.2).abs() < 1e-5,
            "two ticks, one instance (x accumulates 0.1 per tick)"
        );
    }

    #[test]
    fn input_and_context_surface_all_fields() {
        let _test = TEST_LOCK.lock().unwrap();
        {
            let mut mock = crate::bridge::mock();
            mock.gamepad_connected = true;
            mock.gamepad_axis_x = 0.5;
            mock.gamepad_axis_y = -0.25;
            mock.received_messages.push(b"pong".to_vec());
        }

        let ctx = Context;
        let input = ctx.input();
        assert!(input.gamepad_connected);
        assert_eq!(input.gamepad_axis_x, 0.5);
        assert_eq!(input.gamepad_axis_y, -0.25);

        ctx.load_avatar("avatars/blue.glb");
        {
            let mock = crate::bridge::mock();
            assert_eq!(mock.avatar_path.as_deref(), Some("avatars/blue.glb"));
            assert_eq!(mock.sent_messages.len(), 0);
        }

        ctx.send_message("PeerB", b"hello");
        {
            let mock = crate::bridge::mock();
            assert_eq!(mock.sent_messages.len(), 1);
            assert_eq!(mock.sent_messages[0].0, "PeerB");
            assert_eq!(mock.sent_messages[0].1, b"hello");
        }

        let mut buf = [0u8; 16];
        assert_eq!(ctx.receive_message(&mut buf), Some(4));
        assert_eq!(&buf[..4], b"pong");
    }
}
