//! {{name}} — a session-based shooter built on the RustP2P guest SDK.
//!
//! Move with WASD, "shoot" with Space: each shot tags the remote player when
//! they are within range. The score is shown in the host HUD and synced over
//! the P2P link.

use guest_sdk::prelude::*;

/// Movement speed per frame.
const SPEED: f32 = 0.05;
/// Distance at which a shot registers as a tag.
const TAG_DISTANCE: f32 = 2.0;

/// The game state: local position, yaw, and score.
pub struct {{crate_struct}} {
    x: f32,
    z: f32,
    rot_y: f32,
    score: u32,
}

impl Game for {{crate_struct}} {
    fn new() -> Self {
        Self { x: 0.0, z: 0.0, rot_y: 0.0, score: 0 }
    }

    fn tick(&mut self, ctx: &mut Context) {
        let input = ctx.input();

        // 1. Movement (WASD) + rotation (Space / Shift).
        if input.up {
            self.z -= SPEED;
        }
        if input.down {
            self.z += SPEED;
        }
        if input.left {
            self.x -= SPEED;
        }
        if input.right {
            self.x += SPEED;
        }
        if input.action_1 {
            self.rot_y += 0.1;
        }
        if input.action_2 {
            self.rot_y -= 0.1;
        }

        // 2. Move the local avatar and share the pose with remote peers.
        ctx.set_avatar_transform(self.x, 0.0, self.z, self.rot_y);
        ctx.broadcast_pose(self.x, 0.0, self.z, self.rot_y);

        // 3. Shoot: tag the remote player when they are within range.
        for peer in ["PeerA", "PeerB"] {
            if let Some(remote) = ctx.remote_pose(peer) {
                let dx = self.x - remote.x;
                let dz = self.z - remote.z;
                if dx * dx + dz * dz < TAG_DISTANCE * TAG_DISTANCE {
                    self.score = self.score.saturating_add(1);
                    ctx.set_tag_score(self.score);
                    ctx.broadcast_tag_score(self.score);
                }
            }
        }
    }
}

// The SDK generates the `game_tick` C export that dispatches to this game.
#[export({{crate_struct}})]
fn game_tick() {}
