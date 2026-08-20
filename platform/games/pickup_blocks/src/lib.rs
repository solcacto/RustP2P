//! Pickup Blocks — a simple solo game built on the RustP2P guest SDK.
//!
//! Move the avatar with WASD / arrow keys and walk over the glowing blocks to
//! collect them. Each block collected adds one to the score (shown in the
//! HUD). The world wraps around at the edges so you never get stuck.

use guest_sdk::prelude::*;

/// Units per second while a movement key is held (scaled by delta time).
const SPEED: f32 = 5.0;
/// Distance at which a block is picked up.
const PICKUP_DIST: f32 = 0.6;
/// World half-extent (the player wraps around at +/- this value).
const WORLD: f32 = 6.0;

/// A single collectible block.
#[derive(Clone, Copy)]
struct Block {
    x: f32,
    z: f32,
    taken: bool,
}

/// The game state: local position, score, and the block layout.
pub struct PickupBlocks {
    x: f32,
    z: f32,
    rot_y: f32,
    score: u32,
    blocks: [Block; 8],
}

impl PickupBlocks {
    fn reset_blocks() -> [Block; 8] {
        [
            Block { x: 3.0, z: 0.0, taken: false },
            Block { x: -3.0, z: 0.0, taken: false },
            Block { x: 0.0, z: 3.0, taken: false },
            Block { x: 0.0, z: -3.0, taken: false },
            Block { x: 2.0, z: 2.0, taken: false },
            Block { x: -2.0, z: 2.0, taken: false },
            Block { x: 2.0, z: -2.0, taken: false },
            Block { x: -2.0, z: -2.0, taken: false },
        ]
    }
}

impl Game for PickupBlocks {
    fn new() -> Self {
        Self {
            x: 0.0,
            z: 0.0,
            rot_y: 0.0,
            score: 0,
            blocks: Self::reset_blocks(),
        }
    }

    fn tick(&mut self, ctx: &mut Context) {
        let input = ctx.input();
        let dt = ctx.delta_seconds() as f32;

        // 1. Movement: WASD / arrows, scaled by delta time so the avatar moves
        // at a constant speed regardless of the renderer's frame rate. Action 1
        // rotates the avatar so you can look around; pickup is distance-based.
        let step = SPEED * dt;
        if input.up {
            self.z -= step;
        }
        if input.down {
            self.z += step;
        }
        if input.left {
            self.x -= step;
        }
        if input.right {
            self.x += step;
        }
        if input.action_1 {
            self.rot_y += 0.08;
        }
        if input.action_2 {
            self.rot_y -= 0.08;
        }

        // 2. Wrap around at the world edge.
        if self.x > WORLD {
            self.x = -WORLD;
        } else if self.x < -WORLD {
            self.x = WORLD;
        }
        if self.z > WORLD {
            self.z = -WORLD;
        } else if self.z < -WORLD {
            self.z = WORLD;
        }

        // 3. Pick up blocks within reach.
        for block in self.blocks.iter_mut() {
            if block.taken {
                continue;
            }
            let dx = self.x - block.x;
            let dz = self.z - block.z;
            if dx * dx + dz * dz < PICKUP_DIST * PICKUP_DIST {
                block.taken = true;
                self.score = self.score.saturating_add(1);
                ctx.set_tag_score(self.score);
                // Persist the removal in the world's chunk data so the spot
                // stays cleared even after the session ends.
                ctx.save_chunk_edit(block.x, block.z, "block", 0.0);
            }
        }

        // 4. Draw the remaining blocks so they're visible in the scene. Taken
        // blocks are skipped, so they vanish once collected. Each block sits
        // flat on the ground (center at half its height) with a distance-based
        // glow so nearby targets stand out.
        for block in self.blocks.iter() {
            if block.taken {
                continue;
            }
            let dist = ((block.x - self.x) * (block.x - self.x)
                + (block.z - self.z) * (block.z - self.z))
            .sqrt();
            let glow = (1.2 - dist * 0.1).clamp(0.35, 1.0);
            ctx.draw_box(
                block.x,
                0.45,
                block.z,
                0.9,
                0.9,
                0.9,
                0.2 + 0.5 * glow,
                0.7 * glow,
                0.9 * glow,
            );
        }

        // 5. Move the local avatar (no peers in solo mode, so no broadcast).
        ctx.set_avatar_transform(self.x, 0.0, self.z, self.rot_y);

        // 6. Share the pose and score so a later multiplayer session sees us.
        ctx.broadcast_pose(self.x, 0.0, self.z, self.rot_y);
        ctx.broadcast_tag_score(self.score);
    }
}

// The SDK generates the `game_tick` C export that dispatches to this game.
#[export(PickupBlocks)]
fn game_tick() {}