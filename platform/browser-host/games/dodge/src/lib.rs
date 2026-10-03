//! Dodge: the browser-host template game.
//!
//! Top-down arena collector using only portable guest APIs (input,
//! draw_box, score, transforms), so it runs unchanged on the native Bevy
//! host and the browser host. Arena is 0..16 in x and z.

use guest_sdk::prelude::*;

const SPEED: f32 = 6.0;
const COINS: usize = 5;
const PICKUP_DIST: f32 = 0.9;

pub struct Dodge {
    x: f32,
    z: f32,
    score: u32,
    coins: [(f32, f32); COINS],
    rng: u64,
}

impl Dodge {
    fn next_rand(&mut self) -> f32 {
        self.rng = self.rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.rng >> 33) % 140) as f32 / 10.0 + 1.0
    }

    fn respawn_coin(&mut self, i: usize) {
        self.coins[i] = (self.next_rand(), self.next_rand());
    }
}

impl Game for Dodge {
    fn new() -> Self {
        let mut g = Self { x: 8.0, z: 8.0, score: 0, coins: [(0.0, 0.0); COINS], rng: 0x12345678 };
        for i in 0..COINS {
            g.respawn_coin(i);
        }
        g
    }

    fn tick(&mut self, ctx: &mut Context) {
        let dt = ctx.delta_seconds() as f32;
        let input = ctx.input();
        if input.left { self.x -= SPEED * dt; }
        if input.right { self.x += SPEED * dt; }
        if input.up { self.z -= SPEED * dt; }
        if input.down { self.z += SPEED * dt; }
        self.x = self.x.clamp(1.0, 15.0);
        self.z = self.z.clamp(1.0, 15.0);
        for i in 0..COINS {
            let (cx, cz) = self.coins[i];
            let dx = self.x - cx;
            let dz = self.z - cz;
            if dx * dx + dz * dz < PICKUP_DIST * PICKUP_DIST {
                self.score += 1;
                ctx.set_tag_score(self.score);
                self.respawn_coin(i);
            }
        }
        // Floor, player, coins: everything visible is drawn every tick.
        ctx.draw_box(8.0, 0.0, 8.0, 16.0, 0.2, 16.0, 0.15, 0.15, 0.22);
        ctx.draw_box(self.x, 0.6, self.z, 0.8, 1.0, 0.8, 0.2, 0.9, 0.3);
        for (cx, cz) in self.coins {
            ctx.draw_box(cx, 0.4, cz, 0.5, 0.5, 0.5, 0.95, 0.8, 0.2);
        }
        ctx.set_avatar_transform(self.x, 0.0, self.z, 0.0);
        ctx.broadcast_pose(self.x, 0.0, self.z, 0.0);
    }
}

#[export(Dodge)]
fn game_tick() {}
