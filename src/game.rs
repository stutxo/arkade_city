//! Deterministic 1v1 stick-shooter simulation — fully event-sourced.
//!
//! No wall clock anywhere: the game state is a pure function of the ordered
//! event log. A move event steps once in its direction; a fire event spawns
//! a bullet in the shooter's current facing. Bullets advance once per event,
//! so late-arriving inputs change *when* you see a state, never *what* it
//! is. Integer math only.

pub const ARENA_W: i32 = 800;
pub const ARENA_H: i32 = 450;
pub const STEP: i32 = 25;
pub const BULLET_STEP: i32 = 30;
/// Bullet lifetime measured in applied events.
pub const BULLET_TICKS: u64 = 40;
pub const FIRE_COOLDOWN: u64 = 3;
pub const START_AMMO: u32 = 20;
pub const HIT_RADIUS_SQ: i32 = 14 * 14;

/// Direction encoding in move payloads.
pub const DIR_UP: u8 = 0;
pub const DIR_RIGHT: u8 = 1;
pub const DIR_DOWN: u8 = 2;
pub const DIR_LEFT: u8 = 3;

/// A simulation input, extracted from the ordered game log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// Player stepped once in `dir`.
    Move { side: usize, dir: u8 },
    /// Player fired in their current facing direction.
    Fire { side: usize },
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
    /// Number of events applied (logical time).
    pub tick: u64,
    pub pos: [(i32, i32); 2],
    /// Last move direction per side (bullets fly this way).
    pub facing: [u8; 2],
    pub bullets: Vec<Bullet>,
    pub ammo: [u32; 2],
    pub last_fire_tick: [u64; 2],
    pub phase: Phase,
}

/// Integer-exact point-to-segment distance check: true iff the segment
/// (x0,y0)-(x1,y1) passes within `radius_sq` of (px,py).
fn segment_near_point(x0: i32, y0: i32, x1: i32, y1: i32, px: i32, py: i32, radius_sq: i32) -> bool {
    let dx = (x1 - x0) as i64;
    let dy = (y1 - y0) as i64;
    let ax = (px - x0) as i64;
    let ay = (py - y0) as i64;
    let bx = (px - x1) as i64;
    let by = (py - y1) as i64;
    let r = radius_sq as i64;

    let len_sq = dx * dx + dy * dy;
    if len_sq == 0 {
        return ax * ax + ay * ay <= r;
    }
    if ax * dx + ay * dy <= 0 {
        // closest to the start point
        return ax * ax + ay * ay <= r;
    }
    if bx * dx + by * dy >= 0 {
        // closest to the end point
        return bx * bx + by * by <= r;
    }
    // perpendicular distance² = cross² / len² ≤ r²  ⟺  cross² ≤ r² · len²
    let cross = ax * dy - ay * dx;
    cross * cross <= r * len_sq
}

fn dir_delta(dir: u8) -> (i32, i32) {
    match dir {
        DIR_UP => (0, -1),
        DIR_DOWN => (0, 1),
        DIR_LEFT => (-1, 0),
        DIR_RIGHT => (1, 0),
        _ => (0, 0),
    }
}

impl Sim {
    pub fn new() -> Self {
        Self {
            tick: 0,
            pos: [(60, ARENA_H / 2), (ARENA_W - 60, ARENA_H / 2)],
            facing: [DIR_RIGHT, DIR_LEFT],
            bullets: Vec::new(),
            ammo: [START_AMMO; 2],
            // u64::MAX = "never fired" (0 would block the opening shots)
            last_fire_tick: [u64::MAX; 2],
            phase: Phase::Playing,
        }
    }

    /// Apply the ordered event slice. Both clients converge to identical
    /// state whenever they have seen the same events — regardless of when.
    pub fn run(&mut self, inputs: &[Input]) {
        for input in inputs {
            if matches!(self.phase, Phase::Done { .. }) {
                break;
            }
            self.apply(input);
            self.tick += 1;
        }
    }

    fn apply(&mut self, input: &Input) {
        match *input {
            Input::Move { side, dir } => {
                if dir > DIR_LEFT {
                    return; // unknown direction: ignore, keep determinism
                }
                self.facing[side] = dir;
                let (dx, dy) = dir_delta(dir);
                let nx = (self.pos[side].0 + dx * STEP).clamp(10, ARENA_W - 10);
                let ny = (self.pos[side].1 + dy * STEP).clamp(10, ARENA_H - 10);
                self.pos[side] = (nx, ny);
            }
            Input::Fire { side } => {
                if self.ammo[side] == 0 {
                    return;
                }
                let last = self.last_fire_tick[side];
                if last != u64::MAX && self.tick < last + FIRE_COOLDOWN {
                    return;
                }
                self.ammo[side] -= 1;
                self.last_fire_tick[side] = self.tick;
                let (fx, fy) = dir_delta(self.facing[side]);
                self.bullets.push(Bullet {
                    x: self.pos[side].0 + fx * 16,
                    y: self.pos[side].1 + fy * 16,
                    dx: fx * BULLET_STEP,
                    dy: fy * BULLET_STEP,
                    born_tick: self.tick,
                    side,
                });
            }
        }
        self.advance_bullets();
    }

    fn advance_bullets(&mut self) {
        let mut winner = None;
        for b in &mut self.bullets {
            let (x0, y0) = (b.x, b.y);
            b.x += b.dx;
            b.y += b.dy;
            // Swept collision: the bullet's path segment vs the victim's
            // position — otherwise a 30px step can jump over the hitbox.
            let victim = 1 - b.side;
            let (px, py) = self.pos[victim];
            if segment_near_point(x0, y0, b.x, b.y, px, py, HIT_RADIUS_SQ) {
                winner = Some(b.side);
            }
        }
        self.bullets.retain(|b| {
            b.x >= 0
                && b.x <= ARENA_W
                && b.y >= 0
                && b.y <= ARENA_H
                && self.tick < b.born_tick + BULLET_TICKS
        });
        if let Some(side) = winner {
            self.phase = Phase::Done { winner: side };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movement_is_deterministic() {
        let inputs = [
            Input::Move { side: 0, dir: DIR_RIGHT },
            Input::Move { side: 1, dir: DIR_LEFT },
            Input::Move { side: 0, dir: DIR_RIGHT },
        ];
        let mut a = Sim::new();
        a.run(&inputs);
        let mut b = Sim::new();
        b.run(&inputs);
        assert_eq!(a.state_hash(), b.state_hash());
        assert_eq!(a.pos[0], (60 + 2 * STEP, ARENA_H / 2));
        assert_eq!(a.pos[1], (ARENA_W - 60 - STEP, ARENA_H / 2));
    }

    #[test]
    fn bullet_hits_stationary_opponent() {
        // Player 0 fires east from (60,225); player 1 at (740,225) strafes
        // along the firing line (left/right only) to advance bullet time.
        let mut inputs = vec![Input::Fire { side: 0 }];
        for _ in 0..30 {
            inputs.push(Input::Move { side: 1, dir: DIR_LEFT });
            inputs.push(Input::Move { side: 1, dir: DIR_RIGHT });
        }
        let mut sim = Sim::new();
        sim.run(&inputs);
        assert_eq!(sim.phase, Phase::Done { winner: 0 });
        assert_eq!(sim.ammo[0], START_AMMO - 1);
    }

    #[test]
    fn ammo_and_cooldown_enforced() {
        // Move the target off the firing line first, then spam fire: the
        // cooldown (3 events) limits rate; ammo caps total shots at 20.
        let mut inputs = vec![Input::Move { side: 1, dir: DIR_UP }];
        for _ in 0..START_AMMO * 3 {
            inputs.push(Input::Fire { side: 0 });
            // keep the game clock moving without ending the match
            inputs.push(Input::Move { side: 1, dir: DIR_LEFT });
        }
        let mut sim = Sim::new();
        sim.run(&inputs);
        assert_eq!(sim.ammo[0], 0);
        assert!(matches!(sim.phase, Phase::Playing));
    }

    #[test]
    fn unknown_direction_ignored() {
        let mut sim = Sim::new();
        sim.run(&[Input::Move { side: 0, dir: 99 }]);
        assert_eq!(sim.pos[0], (60, ARENA_H / 2));
    }
}
