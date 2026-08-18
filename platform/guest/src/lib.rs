//! The Universal Avatar game guest.
//!
//! Compiled to `wasm32-unknown-unknown` and loaded by the host, this crate
//! implements a Chase/Tag mini-game. On its real target it runs entirely on
//! `core` (no allocator) and talks to the host exclusively through the
//! `extern "C"` bindings below; the host (macOS) target only builds a harmless
//! no-op stub so the workspace still compiles.

#![cfg_attr(target_arch = "wasm32", no_std)]

/// 16-byte pose layout shared with the host's `get_remote_avatar_pose`
/// (four little-endian f32s). Deliberately excludes timestamps so the guest
/// struct matches the wire format exactly.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct AvatarPose {
    /// World-space X translation in units.
    pub x: f32,
    /// World-space Y translation in units.
    pub y: f32,
    /// World-space Z translation in units.
    pub z: f32,
    /// Yaw rotation in radians.
    pub rot_y: f32,
}

impl AvatarPose {
    pub const fn zero() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            rot_y: 0.0,
        }
    }
}

/// Real game logic, compiled only for the wasm32 target the host loads.
#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::AvatarPose;

    // All host functions live in the `env` import module, matching the host's
    // `Linker::func_wrap("env", ...)` registrations.
    #[link(wasm_import_module = "env")]
    extern "C" {
        fn get_frame_count() -> u64;
        fn get_input_move_up() -> u32;
        fn get_input_move_down() -> u32;
        fn get_input_move_left() -> u32;
        fn get_input_move_right() -> u32;
        fn get_input_action_1() -> u32;
        fn get_input_action_2() -> u32;
        fn update_avatar_transform(x: f32, y: f32, z: f32, rot_y: f32);
        fn broadcast_avatar_pose(x: f32, y: f32, z: f32, rot_y: f32);
        fn get_remote_avatar_pose(
            peer_id_ptr: *const u8,
            peer_id_len: usize,
            out_pose_ptr: *mut AvatarPose,
        ) -> i32;
        fn get_movement_axis() -> u32;
        fn set_tag_score(score: u32);
        fn broadcast_tag_score(score: u32);
    }

    /// Units moved per frame while a movement key is held.
    const MOVE_SPEED: f32 = 0.08;
    /// Distance (units) at which a tag registers.
    const TAG_DISTANCE: f32 = 2.0;
    /// Minimum frames between two tags to keep the score readable.
    const SCORE_COOLDOWN_FRAMES: u64 = 15;

    static mut LOCAL_X: f32 = 0.0;
    static mut LOCAL_Z: f32 = 0.0;
    static mut LOCAL_ROT_Y: f32 = 0.0;
    static mut SCORE: u32 = 0;
    static mut LAST_TAG_FRAME: u64 = 0;
    static mut REMOTE_SEEN: u32 = 0;
    static mut INITIALIZED: u32 = 0;

    /// One game tick, called by the host once per rendered frame.
    #[no_mangle]
    pub extern "C" fn game_tick() {
        unsafe {
            init_spawn();
            let frame = get_frame_count();

            // 1. Read input (WASD) into a simple movement vector.
            let speed = MOVE_SPEED;
            if get_input_move_up() == 1 {
                LOCAL_Z -= speed;
            }
            if get_input_move_down() == 1 {
                LOCAL_Z += speed;
            }
            if get_input_move_left() == 1 {
                LOCAL_X -= speed;
            }
            if get_input_move_right() == 1 {
                LOCAL_X += speed;
            }
            if get_input_action_1() == 1 {
                LOCAL_ROT_Y += 0.1;
            }
            if get_input_action_2() == 1 {
                LOCAL_ROT_Y -= 0.1;
            }

            // 2. Update the local visual avatar.
            update_avatar_transform(LOCAL_X, 0.0, LOCAL_Z, LOCAL_ROT_Y);

            // 3. Share our pose with remote peers.
            broadcast_avatar_pose(LOCAL_X, 0.0, LOCAL_Z, LOCAL_ROT_Y);

            // 4. Chase/Tag mechanic: tag when the remote avatar is close.
            if let Some(remote) = read_remote_pose() {
                REMOTE_SEEN = 1;
                let dx = LOCAL_X - remote.x;
                let dz = LOCAL_Z - remote.z;
                let dist_sq = dx * dx + dz * dz;
                if dist_sq < TAG_DISTANCE * TAG_DISTANCE
                    && frame - LAST_TAG_FRAME >= SCORE_COOLDOWN_FRAMES
                {
                    SCORE += 1;
                    LAST_TAG_FRAME = frame;
                    set_tag_score(SCORE);
                    broadcast_tag_score(SCORE);
                }
            }
        }
    }

    /// Returns the local tag score (also surfaced through `set_tag_score`).
    #[no_mangle]
    pub extern "C" fn get_tag_score() -> u32 {
        unsafe { SCORE }
    }

    /// Returns 1 once a remote peer's pose has been observed.
    #[no_mangle]
    pub extern "C" fn remote_pose_seen() -> u32 {
        unsafe { REMOTE_SEEN }
    }

    /// Probes every known peer id and returns the first pose the host reports.
    /// Only remote peers appear in the host's remote-pose map, so the local
    /// peer id is skipped naturally.
    fn read_remote_pose() -> Option<AvatarPose> {
        let peers: [&[u8]; 2] = [b"PeerA", b"PeerB"];
        let mut pose = AvatarPose::zero();
        for peer in peers {
            let found = unsafe { get_remote_avatar_pose(peer.as_ptr(), peer.len(), &mut pose) };
            if found == 1 {
                return Some(pose);
            }
        }
        None
    }

    /// Places the avatar on its role's spawn point on the first tick so the
    /// tag proximity check starts from a meaningful distance.
    fn init_spawn() {
        unsafe {
            if INITIALIZED == 1 {
                return;
            }
            INITIALIZED = 1;
            if get_movement_axis() == 0 {
                // Role A spawns at +X.
                LOCAL_X = 3.0;
            } else {
                // Role B spawns at -X.
                LOCAL_X = -3.0;
            }
        }
    }
}

/// No-op stubs so the workspace still builds for the host (macOS) target; the
/// guest is only ever instantiated by the host as the wasm32 artifact.
#[cfg(not(target_arch = "wasm32"))]
mod native {
    #[no_mangle]
    pub extern "C" fn game_tick() {}

    #[no_mangle]
    pub extern "C" fn get_tag_score() -> u32 {
        0
    }

    #[no_mangle]
    pub extern "C" fn remote_pose_seen() -> u32 {
        0
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::{game_tick, get_tag_score, remote_pose_seen};
#[cfg(not(target_arch = "wasm32"))]
pub use native::{game_tick, get_tag_score, remote_pose_seen};

#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn on_panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}