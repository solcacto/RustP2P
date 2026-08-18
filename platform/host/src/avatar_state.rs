//! Avatar pose types shared between the Wasm guest, the network layer, and the
//! Bevy renderer.

use std::time::Instant;

/// Target pose for the Universal Avatar, written by the Wasm guest through the
/// `update_avatar_transform` host function and applied to the Bevy scene by the
/// renderer.
#[derive(Debug, Clone, Copy)]
pub struct AvatarState {
    /// World-space X translation in units.
    pub x: f32,
    /// World-space Y translation in units.
    pub y: f32,
    /// World-space Z translation in units.
    pub z: f32,
    /// Yaw rotation in radians.
    pub rot_y: f32,
}

impl Default for AvatarState {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            rot_y: 0.0,
        }
    }
}

/// Latest pose of a remote peer's avatar, relayed as a 16-byte payload (four
/// little-endian f32s) and timestamped so the renderer can despawn avatars that
/// stop reporting.
#[derive(Debug, Clone, Copy)]
pub struct AvatarPose {
    /// World-space X translation in units.
    pub x: f32,
    /// World-space Y translation in units.
    pub y: f32,
    /// World-space Z translation in units.
    pub z: f32,
    /// Yaw rotation in radians.
    pub rot_y: f32,
    /// When the pose was last received; used to drop stale remote avatars.
    pub last_seen: Instant,
}

impl Default for AvatarPose {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            rot_y: 0.0,
            last_seen: Instant::now(),
        }
    }
}