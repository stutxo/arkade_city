//! Deterministic, endless maze simulation.
//!
//! Each accepted asset burn is one attempted grid step. Players do not
//! collide, so every player's state depends only on that player's ordered
//! move sequence. Reaching the exit records a lap and returns the dot to the
//! entrance.

pub const MAZE_W: i32 = 21;
pub const MAZE_H: i32 = 21;
pub const START: (i32, i32) = (1, 10);
pub const GOAL: (i32, i32) = (19, 10);

pub const DIR_UP: u8 = 0;
pub const DIR_RIGHT: u8 = 1;
pub const DIR_DOWN: u8 = 2;
pub const DIR_LEFT: u8 = 3;

pub const DIRECTIONS: [char; 4] = ['w', 'd', 's', 'a'];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerState {
    pub x: i32,
    pub y: i32,
    pub moves: u32,
    pub laps: u32,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            x: START.0,
            y: START.1,
            moves: 0,
            laps: 0,
        }
    }
}

impl PlayerState {
    pub fn apply(&mut self, dir: u8) {
        let (dx, dy) = match dir {
            DIR_UP => (0, -1),
            DIR_RIGHT => (1, 0),
            DIR_DOWN => (0, 1),
            DIR_LEFT => (-1, 0),
            _ => return,
        };
        self.moves = self.moves.saturating_add(1);
        let next = (self.x + dx, self.y + dy);
        if !is_wall(next.0, next.1) {
            (self.x, self.y) = next;
        }
        if (self.x, self.y) == GOAL {
            self.laps = self.laps.saturating_add(1);
            (self.x, self.y) = START;
        }
    }
}

/// Four alternating barriers make one readable, deterministic slalom maze.
pub fn is_wall(x: i32, y: i32) -> bool {
    if x < 0 || y < 0 || x >= MAZE_W || y >= MAZE_H {
        return true;
    }
    if x == 0 || y == 0 || x == MAZE_W - 1 || y == MAZE_H - 1 {
        return true;
    }
    match x {
        4 | 12 => y != 3,
        8 | 16 => y != 17,
        _ => false,
    }
}

pub fn walls() -> Vec<[i32; 2]> {
    let mut out = Vec::new();
    for y in 0..MAZE_H {
        for x in 0..MAZE_W {
            if is_wall(x, y) {
                out.push([x, y]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walls_block_steps_but_still_consume_moves() {
        let mut player = PlayerState {
            x: 3,
            y: 10,
            ..Default::default()
        };
        player.apply(DIR_RIGHT);
        assert_eq!((player.x, player.y), (3, 10));
        assert_eq!(player.moves, 1);
    }

    #[test]
    fn route_reaches_exit_and_starts_another_lap() {
        let mut player = PlayerState::default();
        let mut walk = |dir, count| {
            for _ in 0..count {
                player.apply(dir);
            }
        };

        walk(DIR_UP, 7);
        walk(DIR_RIGHT, 4);
        walk(DIR_DOWN, 14);
        walk(DIR_RIGHT, 4);
        walk(DIR_UP, 14);
        walk(DIR_RIGHT, 4);
        walk(DIR_DOWN, 14);
        walk(DIR_RIGHT, 6);
        walk(DIR_UP, 7);

        assert_eq!(player.laps, 1);
        assert_eq!((player.x, player.y), START);
    }

    #[test]
    fn maze_is_square_and_endpoints_are_open() {
        assert_eq!(MAZE_W, MAZE_H);
        assert!(!is_wall(START.0, START.1));
        assert!(!is_wall(GOAL.0, GOAL.1));
    }
}
