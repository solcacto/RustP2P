//! {{name}} — an open-world exploration template built on the RustP2P guest
//! SDK.
//!
//! Move with WASD, press `1`/`2` to switch between avatars at runtime, and
//! press Space to toggle sprint. Shows off input, avatar hot-swapping, pose
//! sync, and the score HUD.

use guest_sdk::prelude::*;

const WALK_SPEED: f32 = 0.04;
const SPRINT_SPEED: f32 = 0.09;

/// The game state: local position, yaw, movement, and score.
pub struct {{crate_struct}} {
    x: f32,
    z: f32,
    rot_y: f32,
    sprinting: bool,
    score: u32,
    avatar_cycle: u32,
}

impl Game for {{crate_struct}} {
    fn new() -> Self {
        Self { x: 0.0, z: 0.0, rot_y: 0.0, sprinting: false, score: 0, avatar_cycle: 0 }
    }

    fn tick(&mut self, ctx: &mut Context) {
        let input = ctx.input();
        let speed = if self.sprinting { SPRINT_SPEED } else { WALK_SPEED };

        // 1. Movement (WASD), rotate (Space / Shift), toggle sprint.
        if input.up {
            self.z -= speed;
        }
        if input.down {
            self.z += speed;
        }
        if input.left {
            self.x -= speed;
        }
        if input.right {
            self.x += speed;
        }
        if input.action_1 {
            self.rot_y += 0.1;
        }
        if input.action_2 {
            self.rot_y -= 0.1;
            self.sprinting = !self.sprinting;
        }

        // 2. Avatar hot-swap: cycle between the standard and blue avatars.
        if self.avatar_cycle == 0 {
            ctx.load_avatar("avatar_standard.glb");
        } else {
            ctx.load_avatar("avatars/blue.glb");
        }
        if input.action_1 {
            self.avatar_cycle = (self.avatar_cycle + 1) % 2;
        }

        // 3. Move the local avatar and share the pose.
        ctx.set_avatar_transform(self.x, 0.0, self.z, self.rot_y);
        ctx.broadcast_pose(self.x, 0.0, self.z, self.rot_y);

        // 4. Score when the remote explorer is close (tag).
        for peer in ["PeerA", "PeerB"] {
            if let Some(remote) = ctx.remote_pose(peer) {
                let dx = self.x - remote.x;
                let dz = self.z - remote.z;
                if dx * dx + dz * dz < 2.5 * 2.5 {
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
