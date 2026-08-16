//! Registry and per-player move ledger.
//!
//! A game-address output registers each issuance and its player script. Native
//! asset burns are then discovered through that script. The burned asset
//! identifies both the player (issuance txid) and direction (group index), and
//! a compact receipt supplies deterministic player-local ordering.

use ark_core::asset::AssetId;
use bitcoin::hashes::Hash;
use bitcoin::opcodes::all::OP_RETURN;
use bitcoin::script::Instruction;
use bitcoin::{OutPoint, Transaction, Txid};
use std::collections::{BTreeMap, HashSet};

pub const GAME_ID: &str = "arkade-arena-v3";
pub const RECEIPT_MAGIC: &[u8; 2] = b"AM";
pub const RECEIPT_VERSION: u8 = 3;
pub const RECEIPT_LEN: usize = 2 + 1 + 4;
pub const PLAYER_ASSET_OUTPUT_INDEX: u16 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetPacket {
    pub groups: Vec<AssetPacketGroup>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetPacketGroup {
    pub asset_id: Option<AssetId>,
    pub has_control_asset: bool,
    pub metadata: Option<Vec<(String, String)>>,
    pub inputs: Vec<AssetAssignment>,
    pub outputs: Vec<AssetAssignment>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetAssignment {
    pub index: u16,
    pub amount: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveReceipt {
    pub sequence: u32,
}

impl MoveReceipt {
    pub fn encode(self) -> [u8; RECEIPT_LEN] {
        let mut out = [0u8; RECEIPT_LEN];
        out[..2].copy_from_slice(RECEIPT_MAGIC);
        out[2] = RECEIPT_VERSION;
        out[3..].copy_from_slice(&self.sequence.to_le_bytes());
        out
    }

    pub fn decode(raw: &[u8]) -> Option<Self> {
        if raw.len() != RECEIPT_LEN || &raw[..2] != RECEIPT_MAGIC || raw[2] != RECEIPT_VERSION {
            return None;
        }
        Some(Self {
            sequence: u32::from_le_bytes(raw[3..7].try_into().ok()?),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveEvent {
    pub txid: Txid,
    pub player: Txid,
    pub sequence: u32,
    pub action: u8,
    pub predecessor: OutPoint,
    pub created_at: Option<i64>,
    pub local_tentative: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveBurn {
    pub receipt: MoveReceipt,
    pub asset_id: AssetId,
    pub predecessor: OutPoint,
    pub preserved_output_indexes: Vec<u16>,
}

/// Extract non-extension OP_RETURN payloads from a virtual transaction.
pub fn payloads_from_tx(tx: &Transaction) -> Vec<Vec<u8>> {
    tx.output
        .iter()
        .filter(|output| output.value == bitcoin::Amount::ZERO)
        .filter_map(|output| {
            let mut instructions = output.script_pubkey.instructions();
            if !matches!(instructions.next(), Some(Ok(Instruction::Op(OP_RETURN)))) {
                return None;
            }
            let Some(Ok(Instruction::PushBytes(bytes))) = instructions.next() else {
                return None;
            };
            let data = bytes.as_bytes();
            if data.starts_with(b"ARK") {
                return None;
            }
            Some(data.to_vec())
        })
        .collect()
}

pub fn receipt_from_tx(tx: &Transaction) -> Option<MoveReceipt> {
    let receipts: Vec<_> = payloads_from_tx(tx)
        .iter()
        .filter_map(|payload| MoveReceipt::decode(payload))
        .collect();
    match receipts.as_slice() {
        [receipt] => Some(*receipt),
        _ => None,
    }
}

/// Arkade's GetAsset endpoint returns metadata as hex-encoded uLEB128 fields.
pub fn decode_asset_metadata(raw: &str) -> Option<Vec<(String, String)>> {
    let bytes: Vec<u8> = bitcoin::hex::FromHex::from_hex(raw).ok()?;
    let mut cursor = 0usize;
    let metadata = read_metadata(&bytes, &mut cursor)?;
    (cursor == bytes.len()).then_some(metadata)
}

fn read_metadata(bytes: &[u8], cursor: &mut usize) -> Option<Vec<(String, String)>> {
    let count = read_uvarint(bytes, cursor)?;
    if count > 1_000 {
        return None;
    }
    let mut metadata = Vec::with_capacity(count.try_into().ok()?);
    for _ in 0..count {
        let key = read_string(bytes, cursor)?;
        let value = read_string(bytes, cursor)?;
        metadata.push((key, value));
    }
    Some(metadata)
}

fn read_string(bytes: &[u8], cursor: &mut usize) -> Option<String> {
    let len: usize = read_uvarint(bytes, cursor)?.try_into().ok()?;
    let end = cursor.checked_add(len)?;
    let text = std::str::from_utf8(bytes.get(*cursor..end)?)
        .ok()?
        .to_string();
    *cursor = end;
    Some(text)
}

fn read_uvarint(bytes: &[u8], cursor: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..=63).step_by(7) {
        let byte = *bytes.get(*cursor)?;
        *cursor += 1;
        if shift == 63 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn read_u16(bytes: &[u8], cursor: &mut usize) -> Option<u16> {
    let end = cursor.checked_add(2)?;
    let value = u16::from_le_bytes(bytes.get(*cursor..end)?.try_into().ok()?);
    *cursor = end;
    Some(value)
}

fn read_asset_id(bytes: &[u8], cursor: &mut usize) -> Option<AssetId> {
    let end = cursor.checked_add(32)?;
    let mut txid_bytes: [u8; 32] = bytes.get(*cursor..end)?.try_into().ok()?;
    *cursor = end;
    txid_bytes.reverse();
    Some(AssetId {
        txid: Txid::from_byte_array(txid_bytes),
        group_index: read_u16(bytes, cursor)?,
    })
}

fn read_control_asset(bytes: &[u8], cursor: &mut usize) -> Option<()> {
    let kind = *bytes.get(*cursor)?;
    *cursor += 1;
    match kind {
        1 => {
            read_asset_id(bytes, cursor)?;
        }
        2 => {
            read_u16(bytes, cursor)?;
        }
        _ => return None,
    }
    Some(())
}

fn read_assignments(bytes: &[u8], cursor: &mut usize) -> Option<Vec<AssetAssignment>> {
    let count = read_uvarint(bytes, cursor)?;
    if count > 10_000 {
        return None;
    }
    let mut assignments = Vec::with_capacity(count.try_into().ok()?);
    for _ in 0..count {
        if *bytes.get(*cursor)? != 1 {
            return None;
        }
        *cursor += 1;
        assignments.push(AssetAssignment {
            index: read_u16(bytes, cursor)?,
            amount: read_uvarint(bytes, cursor)?,
        });
    }
    Some(assignments)
}

fn decode_asset_packet(bytes: &[u8]) -> Option<AssetPacket> {
    let mut cursor = 0usize;
    let count = read_uvarint(bytes, &mut cursor)?;
    if count == 0 || count > 1_000 {
        return None;
    }
    let mut groups = Vec::with_capacity(count.try_into().ok()?);
    for _ in 0..count {
        let presence = *bytes.get(cursor)?;
        cursor += 1;
        if presence & !0x07 != 0 {
            return None;
        }
        let asset_id = if presence & 0x01 != 0 {
            Some(read_asset_id(bytes, &mut cursor)?)
        } else {
            None
        };
        let has_control_asset = presence & 0x02 != 0;
        if has_control_asset {
            read_control_asset(bytes, &mut cursor)?;
        }
        let metadata = if presence & 0x04 != 0 {
            Some(read_metadata(bytes, &mut cursor)?)
        } else {
            None
        };
        groups.push(AssetPacketGroup {
            asset_id,
            has_control_asset,
            metadata,
            inputs: read_assignments(bytes, &mut cursor)?,
            outputs: read_assignments(bytes, &mut cursor)?,
        });
    }
    (cursor == bytes.len()).then_some(AssetPacket { groups })
}

pub fn asset_packet_from_tx(tx: &Transaction) -> Option<AssetPacket> {
    let mut packet = None;
    for output in &tx.output {
        let Some(payload) = ark_core::extension::extension_payload(&output.script_pubkey) else {
            continue;
        };
        for (packet_type, bytes) in ark_core::extension::iter_packets(payload).ok()? {
            if packet_type != 0 {
                continue;
            }
            if packet.is_some() {
                return None;
            }
            packet = Some(decode_asset_packet(bytes)?);
        }
    }
    packet
}

fn action_from_pairs(metadata: &[(String, String)], group_index: u16) -> Option<u8> {
    let action = u8::try_from(group_index).ok()?;
    let expected = crate::game::ACTION_NAMES.get(action as usize)?.to_string();
    (metadata
        == [
            ("game".to_string(), GAME_ID.to_string()),
            ("action".to_string(), expected),
        ])
    .then_some(action)
}

fn is_valid_move_issuance(tx: &Transaction) -> bool {
    let Some(packet) = asset_packet_from_tx(tx) else {
        return false;
    };
    if packet.groups.len() != crate::game::ACTION_COUNT {
        return false;
    }
    packet.groups.iter().enumerate().all(|(index, group)| {
        group.asset_id.is_none()
            && !group.has_control_asset
            && group.inputs.is_empty()
            && group.outputs
                == [AssetAssignment {
                    index: PLAYER_ASSET_OUTPUT_INDEX,
                    amount: crate::game::ACTION_SUPPLIES[index],
                }]
            && group
                .metadata
                .as_deref()
                .and_then(|metadata| action_from_pairs(metadata, index as u16))
                == Some(index as u8)
    })
}

/// Validate a canonical issuance that permanently registers itself at the
/// game script, and return the script carrying the new player's assets.
pub fn registration_player_script(
    tx: &Transaction,
    game_script_hex: &str,
    dust_sats: u64,
) -> Option<String> {
    if !payloads_from_tx(tx).is_empty() || !is_valid_move_issuance(tx) {
        return None;
    }
    let game_outputs: Vec<_> = tx
        .output
        .iter()
        .enumerate()
        .filter(|(_, output)| output.script_pubkey.to_hex_string() == game_script_hex)
        .collect();
    if game_outputs.len() != 1
        || game_outputs[0].0 != 0
        || game_outputs[0].1.value.to_sat() != dust_sats
    {
        return None;
    }
    let player_output = tx.output.get(usize::from(PLAYER_ASSET_OUTPUT_INDEX))?;
    if player_output.value.to_sat() != dust_sats
        || !player_output.script_pubkey.is_p2tr()
        || player_output.script_pubkey.to_hex_string() == game_script_hex
    {
        return None;
    }
    Some(player_output.script_pubkey.to_hex_string())
}

/// Parse a native one-unit move-asset burn. Destination-script checks happen
/// after lookup of the registered player script.
pub fn move_burn_from_tx(tx: &Transaction) -> Option<MoveBurn> {
    let [input] = tx.input.as_slice() else {
        return None;
    };
    let receipt = receipt_from_tx(tx)?;
    let packet = asset_packet_from_tx(tx)?;
    let mut player = None;
    let mut burned = None;
    let mut asset_ids = HashSet::new();
    let mut preserved_output_indexes = Vec::new();

    for group in packet.groups {
        let asset_id = group.asset_id?;
        if group.has_control_asset
            || group.metadata.is_some()
            || group.inputs.is_empty()
            || asset_id.group_index >= crate::game::ACTION_COUNT as u16
            || !asset_ids.insert(asset_id)
        {
            return None;
        }
        match player {
            Some(txid) if txid != asset_id.txid => return None,
            None => player = Some(asset_id.txid),
            _ => {}
        }
        if group
            .inputs
            .iter()
            .any(|input| input.amount == 0 || tx.input.get(usize::from(input.index)).is_none())
            || group.outputs.iter().any(|output| output.amount == 0)
        {
            return None;
        }
        let input_amount = group
            .inputs
            .iter()
            .try_fold(0u64, |sum, input| sum.checked_add(input.amount))?;
        let output_amount = group
            .outputs
            .iter()
            .try_fold(0u64, |sum, output| sum.checked_add(output.amount))?;
        let deficit = input_amount.checked_sub(output_amount)?;
        match deficit {
            0 => {}
            1 if burned.is_none() => burned = Some(asset_id),
            _ => return None,
        }
        for output in group.outputs {
            tx.output.get(usize::from(output.index))?;
            if !preserved_output_indexes.contains(&output.index) {
                preserved_output_indexes.push(output.index);
            }
        }
    }

    Some(MoveBurn {
        receipt,
        asset_id: burned?,
        predecessor: input.previous_output,
        preserved_output_indexes,
    })
}

/// Validate the metadata and canonical group index of a move asset.
pub fn action_from_metadata(raw: &str, group_index: u16) -> Option<u8> {
    let metadata = decode_asset_metadata(raw)?;
    action_from_pairs(&metadata, group_index)
}

/// Select contiguous player streams, then apply the coordinator's global order.
pub fn canonical_actions(events: &[MoveEvent]) -> Vec<MoveEvent> {
    let mut grouped: BTreeMap<Txid, Vec<&MoveEvent>> = BTreeMap::new();
    for event in events {
        grouped.entry(event.player).or_default().push(event);
    }

    let mut out = Vec::new();
    for (player, moves) in grouped {
        let mut expected = 0u32;
        let mut predecessor = OutPoint {
            txid: player,
            vout: u32::from(PLAYER_ASSET_OUTPUT_INDEX),
        };
        loop {
            let event = moves
                .iter()
                .copied()
                .filter(|event| {
                    event.sequence == expected
                        && event.predecessor == predecessor
                        && (event.created_at.is_some() || event.local_tentative)
                })
                .min_by(|left, right| match (&left.created_at, &right.created_at) {
                    (Some(left_time), Some(right_time)) => (left_time, left.txid.to_string())
                        .cmp(&(right_time, right.txid.to_string())),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => left.txid.to_string().cmp(&right.txid.to_string()),
                });
            let Some(event) = event else {
                break;
            };
            out.push(event.clone());
            predecessor = OutPoint {
                txid: event.txid,
                vout: 0,
            };
            expected = expected.saturating_add(1);
        }
    }
    out.sort_by(|left, right| match (&left.created_at, &right.created_at) {
        (Some(left_time), Some(right_time)) => {
            (left_time, left.txid.to_string()).cmp(&(right_time, right.txid.to_string()))
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => left.txid.to_string().cmp(&right.txid.to_string()),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_core::asset::packet::{AssetGroup, AssetInput, AssetOutput, Packet};
    use bitcoin::hashes::Hash;
    use bitcoin::hex::DisplayHex;
    use bitcoin::{absolute, transaction, Amount, ScriptBuf, Transaction, TxIn, TxOut};

    fn txid(byte: u8) -> Txid {
        Txid::from_byte_array([byte; 32])
    }

    fn outpoint(txid: Txid, vout: u32) -> OutPoint {
        OutPoint { txid, vout }
    }

    fn encode_metadata(pairs: &[(&str, &str)]) -> String {
        let mut bytes = vec![pairs.len() as u8];
        for (key, value) in pairs {
            bytes.push(key.len() as u8);
            bytes.extend_from_slice(key.as_bytes());
            bytes.push(value.len() as u8);
            bytes.extend_from_slice(value.as_bytes());
        }
        bytes.to_lower_hex_string()
    }

    fn packet_tx(packet: Packet) -> Transaction {
        Transaction {
            version: transaction::Version::non_standard(3),
            lock_time: absolute::LockTime::ZERO,
            input: Vec::new(),
            output: vec![
                TxOut {
                    value: Amount::from_sat(330),
                    script_pubkey: ScriptBuf::new(),
                },
                packet.to_txout(),
            ],
        }
    }

    fn p2tr_script(byte: u8) -> ScriptBuf {
        let mut bytes = vec![0x51, 0x20];
        bytes.extend_from_slice(&[byte; 32]);
        ScriptBuf::from_bytes(bytes)
    }

    fn receipt_output(sequence: u32) -> TxOut {
        let payload = MoveReceipt { sequence }.encode();
        let data = bitcoin::script::PushBytesBuf::try_from(payload.to_vec()).unwrap();
        TxOut {
            value: Amount::ZERO,
            script_pubkey: bitcoin::script::Builder::new()
                .push_opcode(OP_RETURN)
                .push_slice(&data)
                .into_script(),
        }
    }

    #[test]
    fn receipt_serialization_and_rejection() {
        let receipt = MoveReceipt { sequence: 42 };
        assert_eq!(MoveReceipt::decode(&receipt.encode()), Some(receipt));
        assert_eq!(MoveReceipt::decode(b"AM\x01\0\0\0\0"), None);
        assert_eq!(MoveReceipt::decode(b"short"), None);

        let mut tx = packet_tx(Packet { groups: Vec::new() });
        assert_eq!(receipt_from_tx(&tx), None);
        tx.output.push(receipt_output(1));
        assert_eq!(receipt_from_tx(&tx), Some(MoveReceipt { sequence: 1 }));
        tx.output.push(receipt_output(2));
        assert_eq!(receipt_from_tx(&tx), None);
    }

    #[test]
    fn metadata_identifies_action() {
        let raw = encode_metadata(&[("game", GAME_ID), ("action", "s")]);
        assert_eq!(
            action_from_metadata(&raw, 2),
            Some(crate::game::ACTION_DOWN)
        );
        assert_eq!(action_from_metadata(&raw, 1), None);
        assert_eq!(action_from_metadata("not hex", 2), None);
    }

    #[test]
    fn canonical_stream_requires_contiguous_sequences() {
        let player = txid(9);
        let events = vec![
            MoveEvent {
                txid: txid(4),
                player,
                sequence: 1,
                action: 1,
                predecessor: outpoint(txid(3), 0),
                created_at: Some(2),
                local_tentative: false,
            },
            MoveEvent {
                txid: txid(3),
                player,
                sequence: 0,
                action: 0,
                predecessor: outpoint(player, u32::from(PLAYER_ASSET_OUTPUT_INDEX)),
                created_at: Some(1),
                local_tentative: false,
            },
            MoveEvent {
                txid: txid(2),
                player,
                sequence: 1,
                action: 2,
                predecessor: outpoint(txid(3), 0),
                created_at: Some(2),
                local_tentative: false,
            },
            MoveEvent {
                txid: txid(5),
                player,
                sequence: 3,
                action: 3,
                predecessor: outpoint(txid(2), 0),
                created_at: Some(3),
                local_tentative: false,
            },
        ];
        assert_eq!(
            canonical_actions(&events)
                .iter()
                .map(|event| event.action)
                .collect::<Vec<_>>(),
            vec![0, 2]
        );
    }

    #[test]
    fn canonical_actions_require_time_and_sort_globally() {
        let first_player = txid(1);
        let second_player = txid(2);
        let events = vec![
            MoveEvent {
                txid: txid(8),
                player: first_player,
                sequence: 0,
                action: crate::game::ACTION_SHOOT,
                predecessor: outpoint(first_player, u32::from(PLAYER_ASSET_OUTPUT_INDEX)),
                created_at: Some(20),
                local_tentative: false,
            },
            MoveEvent {
                txid: txid(7),
                player: second_player,
                sequence: 0,
                action: crate::game::ACTION_LEFT,
                predecessor: outpoint(second_player, u32::from(PLAYER_ASSET_OUTPUT_INDEX)),
                created_at: Some(10),
                local_tentative: false,
            },
            MoveEvent {
                txid: txid(9),
                player: second_player,
                sequence: 1,
                action: crate::game::ACTION_SHOOT,
                predecessor: outpoint(txid(7), 0),
                created_at: None,
                local_tentative: false,
            },
            MoveEvent {
                txid: txid(10),
                player: second_player,
                sequence: 2,
                action: crate::game::ACTION_REVIVE,
                predecessor: outpoint(txid(9), 0),
                created_at: Some(30),
                local_tentative: false,
            },
        ];
        let accepted = canonical_actions(&events);
        assert_eq!(
            accepted.iter().map(|event| event.txid).collect::<Vec<_>>(),
            vec![txid(7), txid(8)]
        );
    }

    #[test]
    fn validates_canonical_registration_and_issuance() {
        let groups = crate::game::ACTION_NAMES
            .iter()
            .enumerate()
            .map(|(index, action)| AssetGroup {
                asset_id: None,
                control_asset: None,
                metadata: Some(vec![
                    ("game".to_string(), GAME_ID.to_string()),
                    ("action".to_string(), action.to_string()),
                ]),
                inputs: Vec::new(),
                outputs: vec![AssetOutput {
                    output_index: PLAYER_ASSET_OUTPUT_INDEX,
                    amount: crate::game::ACTION_SUPPLIES[index],
                }],
            })
            .collect();
        let game_script = p2tr_script(1);
        let player_script = p2tr_script(2);
        let mut tx = packet_tx(Packet { groups });
        tx.output.insert(
            0,
            TxOut {
                value: Amount::from_sat(330),
                script_pubkey: game_script.clone(),
            },
        );
        tx.output[1] = TxOut {
            value: Amount::from_sat(330),
            script_pubkey: player_script.clone(),
        };
        assert_eq!(
            registration_player_script(&tx, &game_script.to_hex_string(), 330),
            Some(player_script.to_hex_string())
        );
        let packet = asset_packet_from_tx(&tx).unwrap();
        assert_eq!(packet.groups.len(), crate::game::ACTION_COUNT);
        assert_eq!(
            packet
                .groups
                .iter()
                .map(|group| group.outputs[0].amount)
                .collect::<Vec<_>>(),
            crate::game::ACTION_SUPPLIES
        );
    }

    #[test]
    fn validates_one_unit_protocol_burn() {
        let asset_id = AssetId {
            txid: txid(8),
            group_index: 0,
        };
        let mut tx = packet_tx(Packet {
            groups: vec![AssetGroup {
                asset_id: Some(asset_id),
                control_asset: None,
                metadata: None,
                inputs: vec![AssetInput {
                    input_index: 0,
                    amount: crate::game::ACTION_SUPPLIES[0],
                }],
                outputs: vec![AssetOutput {
                    output_index: 0,
                    amount: crate::game::ACTION_SUPPLIES[0] - 1,
                }],
            }],
        });
        tx.input.push(TxIn {
            previous_output: outpoint(txid(6), 0),
            ..Default::default()
        });
        tx.output.push(receipt_output(7));
        assert_eq!(
            move_burn_from_tx(&tx),
            Some(MoveBurn {
                receipt: MoveReceipt { sequence: 7 },
                asset_id,
                predecessor: outpoint(txid(6), 0),
                preserved_output_indexes: vec![0],
            })
        );
    }
}
