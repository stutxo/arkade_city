//! Game log protocol: OP_RETURN message codec, event collection from
//! virtual transactions, and causal ordering of both players' chains.
//!
//! Wire format (`GM` magic, version 1):
//! ```text
//! "GM" | ver(1) | match(8) | seq(4 LE) | prev(8) | tick(8 LE ms) | kind(1) | data(n)
//! ```
//! `match`/`prev` are the first 8 bytes of the respective txid's internal
//! byte order. `tick` is unix-ms at send time; the sim orders by causal
//! (seq, prev) first, then tick, then txid — fully deterministic.

use bitcoin::opcodes::all::OP_RETURN;
use bitcoin::script::Instruction;
use bitcoin::{Transaction, Txid};
use std::collections::HashMap;

pub const MAGIC: &[u8; 2] = b"GM";
pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 2 + 1 + 8 + 4 + 8 + 8 + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Start,
    Ack,
    Move,
    Fire,
    End,
}

impl Kind {
    fn from_u8(b: u8) -> Option<Self> {
        match b {
            0 => Some(Self::Start),
            1 => Some(Self::Ack),
            2 => Some(Self::Move),
            3 => Some(Self::Fire),
            4 => Some(Self::End),
            _ => None,
        }
    }
    fn to_u8(self) -> u8 {
        match self {
            Self::Start => 0,
            Self::Ack => 1,
            Self::Move => 2,
            Self::Fire => 3,
            Self::End => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Msg {
    pub match_tag: [u8; 8],
    pub seq: u32,
    pub prev: [u8; 8],
    pub tick_ms: u64,
    pub kind: Kind,
    pub data: Vec<u8>,
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.data.len());
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&self.match_tag);
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.prev);
        out.extend_from_slice(&self.tick_ms.to_le_bytes());
        out.push(self.kind.to_u8());
        out.extend_from_slice(&self.data);
        out
    }

    pub fn decode(raw: &[u8]) -> Option<Self> {
        if raw.len() < HEADER_LEN || &raw[..2] != MAGIC || raw[2] != VERSION {
            return None;
        }
        Some(Self {
            match_tag: raw[3..11].try_into().ok()?,
            seq: u32::from_le_bytes(raw[11..15].try_into().ok()?),
            prev: raw[15..23].try_into().ok()?,
            tick_ms: u64::from_le_bytes(raw[23..31].try_into().ok()?),
            kind: Kind::from_u8(raw[31])?,
            data: raw[32..].to_vec(),
        })
    }
}

/// Short reference to a txid: first 8 bytes of its internal byte array.
pub fn txid_tag(txid: &Txid) -> [u8; 8] {
    use bitcoin::hashes::Hash;
    txid.to_byte_array()[..8].try_into().expect("8 bytes")
}

/// Extract all game payloads (plain OP_RETURN outputs) from a virtual tx.
/// Extension (ARK-magic) outputs are skipped; asset packets are not game data.
pub fn payloads_from_tx(tx: &Transaction) -> Vec<Vec<u8>> {
    tx.output
        .iter()
        .filter(|o| o.value == bitcoin::Amount::ZERO)
        .filter_map(|o| {
            let mut instructions = o.script_pubkey.instructions();
            if !matches!(instructions.next(), Some(Ok(Instruction::Op(OP_RETURN)))) {
                return None;
            }
            let Some(Ok(Instruction::PushBytes(bytes))) = instructions.next() else {
                return None;
            };
            let data = bytes.as_bytes();
            // Skip ARK extension outputs (asset packets).
            if data.len() >= 3 && &data[..3] == b"ARK" {
                return None;
            }
            Some(data.to_vec())
        })
        .collect()
}

/// A game event: a decoded message plus the tx that carried it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub txid: Txid,
    /// Which player's script created a VTXO in this tx (0 = us, 1 = opponent).
    pub side: u8,
    pub msg: Msg,
    /// True when the tx also carries an ARK extension output (asset packet).
    pub has_asset_packet: bool,
}

/// Order events deterministically: causal chains per player via (seq),
/// interleaved by tick, ties broken by txid. Both clients compute the
/// identical order from the same set.
pub fn order_events(mut events: Vec<Event>) -> Vec<Event> {
    events.sort_by(|a, b| {
        (a.side, a.msg.seq)
            .cmp(&(b.side, b.msg.seq))
            .then(a.msg.tick_ms.cmp(&b.msg.tick_ms))
            .then(a.txid.cmp(&b.txid))
    });
    // Stable interleave: walk both chains by seq, emitting whichever has the
    // smaller tick; ties broken by side. Deterministic given the same set.
    let mut by_side: [std::collections::VecDeque<Event>; 2] =
        [Default::default(), Default::default()];
    for e in events {
        by_side[e.side as usize].push_back(e);
    }
    let mut out = Vec::new();
    loop {
        let a = by_side[0].front();
        let b = by_side[1].front();
        match (a, b) {
            (None, None) => break,
            (Some(_), None) => out.push(by_side[0].pop_front().unwrap()),
            (None, Some(_)) => out.push(by_side[1].pop_front().unwrap()),
            (Some(x), Some(y)) => {
                let take_a = (x.msg.tick_ms, x.txid) <= (y.msg.tick_ms, y.txid);
                out.push(if take_a {
                    by_side[0].pop_front().unwrap()
                } else {
                    by_side[1].pop_front().unwrap()
                });
            }
        }
    }
    out
}

/// Tracks which virtual txs we've already consumed for a match.
pub struct LogCursor {
    /// VTXO outpoints (as "txid:vout") already processed.
    pub seen_outpoints: std::collections::HashSet<String>,
    /// Virtual txids already fetched and parsed.
    pub seen_txs: HashMap<Txid, ()>,
}

impl Default for LogCursor {
    fn default() -> Self {
        Self::new()
    }
}

impl LogCursor {
    pub fn new() -> Self {
        Self {
            seen_outpoints: Default::default(),
            seen_txs: Default::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_roundtrip() {
        let msg = Msg {
            match_tag: [1, 2, 3, 4, 5, 6, 7, 8],
            seq: 42,
            prev: [0xaa; 8],
            tick_ms: 1_700_000_000_123,
            kind: Kind::Move,
            data: vec![0b0101],
        };
        let decoded = Msg::decode(&msg.encode()).unwrap();
        assert_eq!(decoded.seq, 42);
        assert_eq!(decoded.tick_ms, msg.tick_ms);
        assert_eq!(decoded.kind, Kind::Move);
        assert_eq!(decoded.data, vec![0b0101]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Msg::decode(b"").is_none());
        assert!(Msg::decode(b"GM\x00").is_none());
        assert!(Msg::decode(b"XX\x01........").is_none());
    }

    #[test]
    fn deterministic_interleave() {
        let mk = |side: u8, seq: u32, tick: u64, txid_byte: u8| Event {
            txid: {
                use bitcoin::hashes::Hash;
                Txid::from_byte_array([txid_byte; 32])
            },
            side,
            msg: Msg {
                match_tag: [0; 8],
                seq,
                prev: [0; 8],
                tick_ms: tick,
                kind: Kind::Move,
                data: vec![],
            },
            has_asset_packet: false,
        };
        let a = vec![
            mk(0, 0, 100, 1),
            mk(1, 0, 150, 2),
            mk(0, 1, 200, 3),
            mk(1, 1, 120, 4),
        ];
        let ordered = order_events(a.clone());
        let ticks: Vec<_> = ordered.iter().map(|e| (e.side, e.msg.seq)).collect();
        // Both orderings computed from any permutation must agree.
        let mut shuffled = a;
        shuffled.reverse();
        assert_eq!(order_events(shuffled), ordered);
        // seq order within each side is preserved.
        let a_seqs: Vec<u32> = ordered.iter().filter(|e| e.side == 0).map(|e| e.msg.seq).collect();
        assert_eq!(a_seqs, vec![0, 1]);
        let _ = ticks;
    }
}
