//! Deterministic finite arena simulation.

use bitcoin::hashes::Hash;
use bitcoin::Txid;
use std::collections::BTreeMap;

pub const ARENA_W: i32 = 21;
pub const ARENA_H: i32 = 21;
pub const MAX_HP: u8 = 3;
const MAX_SHOT_TRACES: usize = 64;

pub const ACTION_UP: u8 = 0;
pub const ACTION_RIGHT: u8 = 1;
pub const ACTION_DOWN: u8 = 2;
pub const ACTION_LEFT: u8 = 3;
pub const ACTION_SHOOT: u8 = 4;
pub const ACTION_REVIVE: u8 = 5;
pub const ACTION_COUNT: usize = 6;
pub const ACTION_NAMES: [&str; ACTION_COUNT] = ["w", "d", "s", "a", "bullet", "life"];
pub const ACTION_SUPPLIES: [u64; ACTION_COUNT] = [50, 50, 50, 50, 50, 5];

// Filled footprints make houses visible and ensure they behave as solid cover.
const HOUSES: [(i32, i32, i32, i32); 4] =
    [(3, 3, 3, 3), (15, 3, 3, 3), (3, 15, 3, 3), (15, 15, 3, 3)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerState {
    pub x: i32,
    pub y: i32,
    pub facing: u8,
    pub hp: u8,
    pub kills: u32,
}

impl PlayerState {
    pub fn spawn(id: Txid) -> Self {
        let (x, y) = spawn_cell(id);
        Self {
            x,
            y,
            facing: ACTION_RIGHT,
            hp: MAX_HP,
            kills: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimedArenaAction {
    pub txid: Txid,
    pub player: Txid,
    pub action: u8,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShotTrace {
    pub id: String,
    pub shooter: String,
    pub start: [i32; 2],
    pub end: [i32; 2],
    pub hit_player: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArenaReplay {
    pub players: BTreeMap<Txid, PlayerState>,
    pub shot_traces: Vec<ShotTrace>,
}

pub fn replay(
    players: impl IntoIterator<Item = Txid>,
    actions: &[TimedArenaAction],
) -> ArenaReplay {
    let mut states: BTreeMap<_, _> = players
        .into_iter()
        .map(|id| (id, PlayerState::spawn(id)))
        .collect();
    let mut shot_traces = Vec::new();
    let mut ordered: Vec<_> = actions
        .iter()
        .filter(|action| states.contains_key(&action.player))
        .copied()
        .collect();
    ordered.sort_by_key(action_order);
    for action in ordered {
        apply_action(&mut states, action, &mut shot_traces);
        if shot_traces.len() > MAX_SHOT_TRACES {
            shot_traces.drain(..shot_traces.len() - MAX_SHOT_TRACES);
        }
    }

    ArenaReplay {
        players: states,
        shot_traces,
    }
}

fn action_order(action: &TimedArenaAction) -> (i64, String) {
    (action.created_at, action.txid.to_string())
}

fn apply_action(
    states: &mut BTreeMap<Txid, PlayerState>,
    event: TimedArenaAction,
    traces: &mut Vec<ShotTrace>,
) {
    if event.action <= ACTION_LEFT {
        let player = states.get_mut(&event.player).expect("known player exists");
        if player.hp == 0 {
            return;
        }
        player.facing = event.action;
        let (dx, dy) = direction_delta(event.action);
        let next = (player.x + dx, player.y + dy);
        if !is_wall(next.0, next.1) {
            (player.x, player.y) = next;
        }
    } else if event.action == ACTION_SHOOT {
        let shooter = states[&event.player];
        if shooter.hp == 0 {
            return;
        }
        let (dx, dy) = direction_delta(shooter.facing);
        let (mut x, mut y) = (shooter.x + dx, shooter.y + dy);
        let target = loop {
            if is_wall(x, y) {
                break None;
            }
            let target = states
                .iter()
                .filter(|(id, player)| {
                    **id != event.player && player.hp > 0 && player.x == x && player.y == y
                })
                .min_by_key(|(id, _)| id.to_string())
                .map(|(id, _)| *id);
            if target.is_some() {
                break target;
            }
            x += dx;
            y += dy;
        };
        if let Some(victim) = target {
            let hp = states[&victim].hp;
            states.get_mut(&victim).expect("target exists").hp = hp.saturating_sub(1);
            if hp == 1 {
                states.get_mut(&event.player).expect("shooter exists").kills =
                    states[&event.player].kills.saturating_add(1);
            }
        }
        traces.push(ShotTrace {
            id: event.txid.to_string(),
            shooter: event.player.to_string(),
            start: [shooter.x, shooter.y],
            end: [x, y],
            hit_player: target.map(|id| id.to_string()),
        });
    } else if event.action == ACTION_REVIVE {
        let current = states[&event.player];
        if current.hp == 0 {
            let mut revived = PlayerState::spawn(event.player);
            revived.kills = current.kills;
            states.insert(event.player, revived);
        }
    }
}

fn direction_delta(direction: u8) -> (i32, i32) {
    match direction {
        ACTION_UP => (0, -1),
        ACTION_RIGHT => (1, 0),
        ACTION_DOWN => (0, 1),
        ACTION_LEFT => (-1, 0),
        _ => (0, 0),
    }
}

pub fn is_house(x: i32, y: i32) -> bool {
    HOUSES
        .iter()
        .any(|(hx, hy, width, height)| x >= *hx && x < hx + width && y >= *hy && y < hy + height)
}

pub fn is_wall(x: i32, y: i32) -> bool {
    if x <= 0 || y <= 0 || x >= ARENA_W - 1 || y >= ARENA_H - 1 {
        return true;
    }
    is_house(x, y)
        || (y == 7 && (2..=8).contains(&x) && x != 5)
        || (x == 10 && (2..=8).contains(&y) && y != 5)
        || (y == 13 && (12..=18).contains(&x) && x != 15)
        || (x == 10 && (12..=18).contains(&y) && y != 15)
        || matches!(
            (x, y),
            (7, 10) | (8, 10) | (12, 10) | (13, 10) | (10, 9) | (10, 11)
        )
}

pub fn spawn_cell(id: Txid) -> (i32, i32) {
    let cells: Vec<_> = (2..ARENA_H - 2)
        .flat_map(|y| (2..ARENA_W - 2).map(move |x| (x, y)))
        .filter(|(x, y)| !is_wall(*x, *y))
        .collect();
    let bytes = id.to_byte_array();
    let hash = bytes.iter().fold(0usize, |value, byte| {
        value.wrapping_mul(257).wrapping_add(*byte as usize)
    });
    cells[hash % cells.len()]
}

pub fn walls() -> Vec<[i32; 2]> {
    cells_matching(is_wall)
}

pub fn houses() -> Vec<[i32; 2]> {
    cells_matching(is_house)
}

fn cells_matching(predicate: impl Fn(i32, i32) -> bool) -> Vec<[i32; 2]> {
    let mut cells = Vec::new();
    for y in 0..ARENA_H {
        for x in 0..ARENA_W {
            if predicate(x, y) {
                cells.push([x, y]);
            }
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> Txid {
        Txid::from_byte_array([byte; 32])
    }

    fn state(x: i32, y: i32, facing: u8) -> PlayerState {
        PlayerState {
            x,
            y,
            facing,
            hp: MAX_HP,
            kills: 0,
        }
    }

    fn action(tx: u8, player: u8, kind: u8, created_at: i64) -> TimedArenaAction {
        TimedArenaAction {
            txid: id(tx),
            player: id(player),
            action: kind,
            created_at,
        }
    }

    #[test]
    fn deterministic_spawn_is_open_and_initially_faces_right() {
        assert_eq!(PlayerState::spawn(id(4)), PlayerState::spawn(id(4)));
        let player = PlayerState::spawn(id(4));
        assert!(!is_wall(player.x, player.y));
        assert_eq!(player.facing, ACTION_RIGHT);
        assert_eq!(player.hp, MAX_HP);
    }

    #[test]
    fn replays_every_action_in_global_timestamp_and_txid_order() {
        let player = id(9);
        let spawn = spawn_cell(player);
        let result = replay(
            [player],
            &[
                action(3, 9, ACTION_DOWN, 15),
                action(2, 9, ACTION_LEFT, 10),
                action(1, 9, ACTION_UP, 10),
            ],
        );
        assert_eq!(result.players[&player].facing, ACTION_DOWN);
        assert_eq!(
            (result.players[&player].x, result.players[&player].y),
            [ACTION_UP, ACTION_LEFT, ACTION_DOWN]
                .into_iter()
                .fold(spawn, |position, action| {
                    let delta = direction_delta(action);
                    let next = (position.0 + delta.0, position.1 + delta.1);
                    if is_wall(next.0, next.1) {
                        position
                    } else {
                        next
                    }
                })
        );
    }

    #[test]
    fn actions_are_sequential_and_shots_use_current_state() {
        let mover = id(1);
        let shooter = id(2);
        let mut states = BTreeMap::from([
            (mover, state(3, 2, ACTION_UP)),
            (shooter, state(2, 2, ACTION_RIGHT)),
        ]);
        let mut traces = Vec::new();
        apply_action(&mut states, action(2, 2, ACTION_SHOOT, 1), &mut traces);
        apply_action(&mut states, action(1, 1, ACTION_UP, 1), &mut traces);
        assert_eq!(states[&mover].hp, MAX_HP - 1);

        let mut states = BTreeMap::from([
            (mover, state(3, 2, ACTION_UP)),
            (shooter, state(2, 2, ACTION_RIGHT)),
        ]);
        apply_action(&mut states, action(1, 1, ACTION_UP, 1), &mut traces);
        apply_action(&mut states, action(2, 2, ACTION_SHOOT, 1), &mut traces);
        assert_eq!(states[&mover].hp, MAX_HP);
    }

    #[test]
    fn shot_then_revive_preserves_kills() {
        let shooter = id(1);
        let victim = id(2);
        let mut target = state(3, 2, ACTION_LEFT);
        target.hp = 1;
        target.kills = 4;
        let mut states = BTreeMap::from([(shooter, state(2, 2, ACTION_RIGHT)), (victim, target)]);
        let mut traces = Vec::new();
        apply_action(&mut states, action(1, 1, ACTION_SHOOT, 1), &mut traces);
        apply_action(&mut states, action(2, 2, ACTION_REVIVE, 1), &mut traces);
        assert_eq!(states[&shooter].kills, 1);
        assert_eq!(states[&victim].hp, MAX_HP);
        assert_eq!(states[&victim].facing, ACTION_RIGHT);
        assert_eq!(states[&victim].kills, 4);
        assert_eq!((states[&victim].x, states[&victim].y), spawn_cell(victim));
    }

    #[test]
    fn miss_trace_ends_on_cover() {
        let shooter = id(1);
        let mut states = BTreeMap::from([(shooter, state(2, 3, ACTION_RIGHT))]);
        let shot = action(7, 1, ACTION_SHOOT, 1);
        let mut traces = Vec::new();
        apply_action(&mut states, shot, &mut traces);
        assert_eq!(traces[0].id, shot.txid.to_string());
        assert_eq!(traces[0].start, [2, 3]);
        assert_eq!(traces[0].end, [3, 3]);
        assert_eq!(traces[0].hit_player, None);
    }
}
