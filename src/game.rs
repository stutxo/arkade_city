//! Deterministic 1v1 stick-shooter simulation.
//!
//! Integer math only: the same event log produces bit-identical state on any
//! machine. Movement is a WASD bitmask; facing derives from the last nonzero
//! movement direction, so shots need no aim data. One hit wins the match.

pub const TICK_MS: u64 = 50; // 20 ticks per second
pub const ARENA_W: i32 = 800;
pub const ARENA_H: i32 = 450;
pub const PLAYER_SPEED: i32 = 4;
pub const PLAYER_SPEED_DIAG: i32 = 3;
pub const BULLET_SPEED: i32 = 10;
pub const BULLET_SPEED_DIAG: i32 = 7;
pub const BULLET_LIFE_TICKS: u64 = 100;
pub const FIRE_COOLDOWN_TICKS: u64 = 15;
pub const HIT_RADIUS_SQ: i32 = 14 * 14;
pub const START_AMMO: u32 = 20;

pub const KEY_W: u8 = 1;
pub const KEY_A: u8 = 2;
pub const KEY_S: u8 = 4;
pub const KEY_D: u8 = 8;

/// A simulation input, extracted from the ordered game log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Player's pressed-key bitmask effective from this tick.
    Move { side: usize, tick: u64, keys: u8 },
    /// Player fired at this tick (facing/position recomputed by the sim).
    Fire { side: usize, tick: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bullet {
    pub x: i32,
    pub y: i32,
    pub dx: i32,
    pub dy: i32,
    pub born_tick: u64,
    pub side: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Playing,
    Done { winner: usize },
}

#[derive(Clone, Debug)]
pub struct Sim {
    pub tick: u64,
    pub pos: [(i32, i32); 2],
    pub keys: [u8; 2],
    pub facing: [(i32, i32); 2],
    pub bullets: Vec<Bullet>,
    pub ammo: [u32; 2],
    pub last_fire_tick: [u64; 2],
    pub phase: Phase,
    /// Tick the match starts at (both players spawn, sim ignores earlier time).
    pub start_tick: u64,
}

impl Sim {
    pub fn new(start_tick: u64) -> Self {
        Self {
            tick: start_tick,
            pos: [(60, ARENA_H / 2), (ARENA_W - 60, ARENA_H / 2)],
            keys: [0; 2],
            facing: [(1, 0), (-1, 0)],
            bullets: Vec::new(),
            ammo: [START_AMMO; 2],
            last_fire_tick: [0; 2],
            phase: Phase::Playing,
            start_tick,
        }
    }

    /// Advance the simulation to `target_tick`, applying `inputs` in order.
    /// `inputs` must be sorted by tick (stable); events at or before the
    /// current tick are applied immediately.
    pub fn run(&mut self, inputs: &[Input], target_tick: u64) {
        let mut idx = 0;
        while self.tick <= target_tick {
            // Apply all inputs scheduled at or before this tick.
            while idx < inputs.len() && input_tick(&inputs[idx]) <= self.tick {
                self.apply(&inputs[idx]);
                idx += 1;
            }
            if matches!(self.phase, Phase::Done { .. }) {
                break;
            }
            self.step();
            self.tick += 1;
        }
        // Flush remaining inputs due at the target boundary.
        while idx < inputs.len() && input_tick(&inputs[idx]) <= target_tick {
            self.apply(&inputs[idx]);
            idx += 1;
        }
    }

    fn apply(&mut self, input: &Input) {
        match *input {
            Input::Move { side, keys, .. } => {
                self.keys[side] = keys;
                let (dx, dy) = dir_of(keys);
                if dx != 0 || dy != 0 {
                    self.facing[side] = (dx, dy);
                }
            }
            Input::Fire { side, tick } => {
                if self.ammo[side] == 0 {
                    return;
                }
                if tick < self.last_fire_tick[side] + FIRE_COOLDOWN_TICKS
                    && self.last_fire_tick[side] != 0
                {
                    return;
                }
                self.ammo[side] -= 1;
                self.last_fire_tick[side] = tick;
                let (fx, fy) = self.facing[side];
                let (dx, dy) = bullet_velocity(fx, fy);
                self.bullets.push(Bullet {
                    x: self.pos[side].0 + fx * 16,
                    y: self.pos[side].1 + fy * 16,
                    dx,
                    dy,
                    born_tick: tick,
                    side,
                });
            }
        }
    }

    fn step(&mut self) {
        // Players.
        for side in 0..2 {
            let (dx, dy) = dir_of(self.keys[side]);
            let (sx, sy) = if dx != 0 && dy != 0 {
                (PLAYER_SPEED_DIAG, PLAYER_SPEED_DIAG)
            } else {
                (PLAYER_SPEED, PLAYER_SPEED)
            };
            let nx = (self.pos[side].0 + dx * sx).clamp(10, ARENA_W - 10);
            let ny = (self.pos[side].1 + dy * sy).clamp(10, ARENA_H - 10);
            self.pos[side] = (nx, ny);
        }
        // Bullets.
        let mut hits: Option<usize> = None;
        for b in &mut self.bullets {
            b.x += b.dx;
            b.y += b.dy;
            let victim = 1 - b.side;
            let ddx = b.x - self.pos[victim].0;
            let ddy = b.y - self.pos[victim].1;
            if ddx * ddx + ddy * ddy <= HIT_RADIUS_SQ {
                hits = Some(b.side);
            }
        }
        self.bullets.retain(|b| {
            b.x >= 0
                && b.x <= ARENA_W
                && b.y >= 0
                && b.y <= ARENA_H
                && self.tick < b.born_tick + BULLET_LIFE_TICKS
                && hits != Some(b.side)
        });
        if let Some(winner) = hits {
            self.phase = Phase::Done { winner };
            self.bullets.clear();
        }
    }

    /// Deterministic hash of the current state (FNV-1a over fixed fields).
    pub fn state_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        let mut mix = |v: i64| {
            h ^= v as u64;
            h = h.wrapping_mul(0x100000001b3);
        };
        mix(self.tick as i64);
        for side in 0..2 {
            mix(self.pos[side].0 as i64);
            mix(self.pos[side].1 as i64);
            mix(self.ammo[side] as i64);
        }
        for b in &self.bullets {
            mix(b.x as i64);
            mix(b.y as i64);
        }
        h
    }
}

fn input_tick(input: &Input) -> u64 {
    match *input {
        Input::Move { tick, .. } | Input::Fire { tick, .. } => tick,
    }
}

/// WASD bitmask -> (dx, dy) direction in {-1, 0, 1}.
pub fn dir_of(keys: u8) -> (i32, i32) {
    let mut dx = 0;
    let mut dy = 0;
    if keys & KEY_W != 0 {
        dy -= 1;
    }
    if keys & KEY_S != 0 {
        dy += 1;
    }
    if keys & KEY_A != 0 {
        dx -= 1;
    }
    if keys & KEY_D != 0 {
        dx += 1;
    }
    (dx, dy)
}

fn bullet_velocity(fx: i32, fy: i32) -> (i32, i32) {
    if fx != 0 && fy != 0 {
        (fx * BULLET_SPEED_DIAG, fy * BULLET_SPEED_DIAG)
    } else {
        (fx * BULLET_SPEED, fy * BULLET_SPEED)
    }
}

/// Current wall-clock tick.
pub fn tick_of_unix_ms(ms: u64) -> u64 {
    ms / TICK_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movement_is_deterministic() {
        let inputs = [
            Input::Move { side: 0, tick: 10, keys: KEY_D },
            Input::Move { side: 1, tick: 12, keys: KEY_A },
        ];
        let mut a = Sim::new(0);
        a.run(&inputs, 100);
        let mut b = Sim::new(0);
        b.run(&inputs, 100);
        assert_eq!(a.state_hash(), b.state_hash());
        assert!(a.pos[0].0 > 60);
        assert!(a.pos[1].0 < ARENA_W - 60);
    }

    #[test]
    fn straight_shot_hits() {
        // Player 0 at (60, 225) faces right; player 1 idle at (740, 225).
        // Fire at tick 5; bullet needs ~73 ticks to cross. One hit wins.
        let inputs = [Input::Fire { side: 0, tick: 5 }];
        let mut sim = Sim::new(0);
        sim.run(&inputs, 200);
        assert_eq!(sim.phase, Phase::Done { winner: 0 });
    }

    #[test]
    fn ammo_is_enforced() {
        let mut inputs = Vec::new();
        for i in 0..25u64 {
            inputs.push(Input::Fire { side: 0, tick: 10 + i * (FIRE_COOLDOWN_TICKS + 1) });
        }
        let mut sim = Sim::new(0);
        sim.run(&inputs, 10 + 25 * (FIRE_COOLDOWN_TICKS + 1) + BULLET_LIFE_TICKS + 10);
        assert_eq!(sim.ammo[0], 0);
        // 20 bullets fired, no more.
        assert!(matches!(sim.phase, Phase::Done { winner: 0 }));
    }
}
