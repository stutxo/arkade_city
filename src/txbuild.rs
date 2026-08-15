//! Offchain transaction engine: builds, signs and submits Arkade
//! transactions using `ark-core` builders and the REST client.
//!
//! The flow mirrors the SDK client: build -> sign ark inputs -> submit ->
//! (server cosigns) -> sign checkpoints -> finalize.

use crate::arkade::{ArkadeRest, ServerParams, VtxoRecord};
use crate::keys::Keys;
use anyhow::{anyhow, Result};
use ark_core::asset::AssetId;
use ark_core::send::{
    build_asset_burn_transactions, build_asset_send_transactions, build_offchain_transactions,
    sign_ark_transaction, sign_checkpoint_transaction,
};
use ark_core::server;
use ark_core::Asset;
use ark_core::Vtxo;
use bitcoin::hashes::Hash;
use bitcoin::opcodes::all::OP_RETURN;
use bitcoin::psbt;
use bitcoin::secp256k1::{schnorr, Message};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin::{Amount, Psbt, TapLeafHash, TapSighashType, TxOut, Txid, XOnlyPublicKey};
use std::str::FromStr;

pub const MOVE_SUPPLY: u64 = crate::gamelog::MOVE_SUPPLY;

#[derive(Debug, Clone)]
pub struct PendingFinalize {
    pub txid: Txid,
    pub signed_ark: Psbt,
    pub checkpoints: Vec<Psbt>,
    pub last_error: String,
}

#[derive(Debug, Clone)]
pub struct UnknownSubmission {
    pub txid: Txid,
    pub signed_ark: Psbt,
    pub checkpoints: Vec<Psbt>,
    pub last_error: String,
}

#[derive(Debug, Clone)]
pub enum RunTxStatus {
    Finalized(Txid),
    Pending(PendingFinalize),
    SubmissionUnknown(UnknownSubmission),
}

impl RunTxStatus {
    pub fn txid(&self) -> Txid {
        match self {
            Self::Finalized(txid) => *txid,
            Self::Pending(pending) => pending.txid,
            Self::SubmissionUnknown(submission) => submission.txid,
        }
    }
}

/// Everything the builders need from `server::Info`, reconstructed from the
/// REST `/v1/info` payload. Fields the builders never read are placeholders.
pub fn server_info(params: &ServerParams) -> server::Info {
    let signer_pk = params
        .signer_pk
        .public_key(bitcoin::secp256k1::Parity::Even);
    server::Info {
        version: String::new(),
        signer_pk,
        forfeit_pk: params.forfeit_pk.inner,
        forfeit_address: params.forfeit_address.clone(),
        checkpoint_tapscript: params.checkpoint_tapscript.clone(),
        network: params.network,
        session_duration: 60,
        unilateral_exit_delay: params.unilateral_exit_delay,
        boarding_exit_delay: params.unilateral_exit_delay,
        utxo_min_amount: None,
        utxo_max_amount: None,
        vtxo_min_amount: Some(Amount::from_sat(params.vtxo_min_sats)),
        vtxo_max_amount: None,
        dust: Amount::from_sat(params.dust_sats),
        fees: None,
        scheduled_session: None,
        deprecated_signers: Vec::new(),
        service_status: Default::default(),
        digest: String::new(),
        max_tx_weight: 40_000,
        max_op_return_outputs: params.max_op_return_outputs,
    }
}

/// Register a player and issue all four direction assets in one transaction.
/// Output zero is the permanent game-registry entry; output one is an exact
/// dust carrier that can be recreated by every later protocol burn.
pub fn build_move_asset_issuance_tx(
    keys: &Keys,
    params: &ServerParams,
    info: &server::Info,
    game_address: ark_core::ArkAddress,
    records: &[VtxoRecord],
) -> Result<(Psbt, Vec<Psbt>, [AssetId; 4])> {
    if records.is_empty() {
        return Err(anyhow!("cannot issue move assets without a VTXO"));
    }
    if records.iter().any(|record| !record.assets.is_empty()) {
        return Err(anyhow!("move asset issuance requires BTC-only inputs"));
    }

    let vtxo = player_vtxo(keys, params)?;
    let own_address = vtxo.to_ark_address();
    let inputs: Vec<_> = records
        .iter()
        .map(|record| vtxo_input(record, &vtxo))
        .collect::<Result<_>>()?;
    let total: Amount = inputs.iter().map(|input| input.amount()).sum();
    let required = Amount::from_sat(params.dust_sats.saturating_mul(2));
    if total < required {
        return Err(anyhow!(
            "player registration requires at least {} sats",
            required.to_sat()
        ));
    }
    let receivers = [
        ark_core::send::SendReceiver::bitcoin(game_address, Amount::from_sat(params.dust_sats)),
        ark_core::send::SendReceiver::bitcoin(own_address, Amount::from_sat(params.dust_sats)),
    ];
    let mut txs = build_offchain_transactions(&receivers, &own_address, &inputs, info)
        .map_err(|e| anyhow!("build player registration: {e}"))?;

    let groups = crate::game::DIRECTIONS
        .iter()
        .map(|direction| ark_core::asset::packet::AssetGroup {
            asset_id: None,
            control_asset: None,
            metadata: Some(vec![
                ("game".to_string(), crate::gamelog::GAME_ID.to_string()),
                ("move".to_string(), direction.to_string()),
            ]),
            inputs: Vec::new(),
            outputs: vec![ark_core::asset::packet::AssetOutput {
                output_index: crate::gamelog::PLAYER_ASSET_OUTPUT_INDEX,
                amount: MOVE_SUPPLY,
            }],
        })
        .collect();
    ark_core::asset::packet::add_asset_packet_to_psbt(
        &mut txs.ark_tx,
        &ark_core::asset::packet::Packet { groups },
    )
    .map_err(|e| anyhow!("attach move asset packet: {e}"))?;

    let txid = txs.ark_tx.unsigned_tx.compute_txid();
    let asset_ids = std::array::from_fn(|group_index| AssetId {
        txid,
        group_index: group_index as u16,
    });
    Ok((txs.ark_tx, txs.checkpoint_txs, asset_ids))
}

/// Burn one direction asset while recreating the player's dust carrier.
pub fn build_move_burn_tx(
    keys: &Keys,
    params: &ServerParams,
    info: &server::Info,
    input: &VtxoRecord,
    asset_id: AssetId,
    sequence: u32,
) -> Result<(Psbt, Vec<Psbt>)> {
    if input.amount_sats != params.dust_sats {
        return Err(anyhow!(
            "move asset carrier must contain exactly {} sats",
            params.dust_sats
        ));
    }
    let vtxo = player_vtxo(keys, params)?;
    let own_address = vtxo.to_ark_address();
    let inputs = [vtxo_input(input, &vtxo)?];
    let mut txs =
        build_asset_burn_transactions(&own_address, &own_address, &inputs, info, asset_id, 1)
            .map_err(|e| anyhow!("build move asset burn: {e}"))?;
    let receipt = crate::gamelog::MoveReceipt { sequence }.encode();
    attach_op_return(&mut txs.ark_tx, &receipt)?;
    Ok((txs.ark_tx, txs.checkpoint_txs))
}

/// The player's default VTXO contract (2-of-2 forfeit + CSV exit leaves).
pub fn player_vtxo(keys: &Keys, params: &ServerParams) -> Result<Vtxo> {
    Vtxo::new_default(
        &keys.secp,
        params.signer_pk,
        keys.owner_pk(),
        params.unilateral_exit_delay,
        params.network,
    )
    .map_err(|e| anyhow!("build vtxo: {e}"))
}

/// Convert an indexer VTXO record into a spendable [`VtxoInput`].
/// Only valid for VTXOs on the player's own default contract.
pub fn vtxo_input(record: &VtxoRecord, vtxo: &Vtxo) -> Result<ark_core::send::VtxoInput> {
    let (spend_script, control_block) = vtxo
        .forfeit_spend_info()
        .map_err(|e| anyhow!("forfeit spend info: {e}"))?;
    let assets = record
        .assets
        .iter()
        .filter_map(|(id, amount)| {
            let (txid, group_index) = parse_asset_id(id)?;
            Some(Asset {
                asset_id: AssetId { txid, group_index },
                amount: *amount,
            })
        })
        .collect();
    Ok(ark_core::send::VtxoInput::new(
        spend_script,
        None,
        control_block,
        vtxo.tapscripts(),
        vtxo.script_pubkey(),
        Amount::from_sat(record.amount_sats),
        record.outpoint,
        assets,
    ))
}

fn parse_asset_id(s: &str) -> Option<(Txid, u16)> {
    parse_asset_id_pub(s).map(|a| (a.txid, a.group_index))
}

/// Public for examples/tests.
pub fn parse_asset_id_pub(s: &str) -> Option<ark_core::asset::AssetId> {
    parse_asset_id_inner(s)
}

fn parse_asset_id_inner(s: &str) -> Option<ark_core::asset::AssetId> {
    if s.len() != 68 {
        return None;
    }
    let txid = Txid::from_str(&s[..64]).ok()?;
    let bytes: [u8; 2] = bitcoin::hex::FromHex::from_hex(&s[64..]).ok()?;
    Some(ark_core::asset::AssetId {
        txid,
        group_index: u16::from_le_bytes(bytes),
    })
}

/// Build a plain (non-extension) OP_RETURN output: `OP_RETURN <payload>`.
/// Zero value; carries game data. Counts toward `max_op_return_outputs`.
pub fn op_return_txout(payload: &[u8]) -> Result<TxOut> {
    let data = bitcoin::script::PushBytesBuf::try_from(payload.to_vec())
        .map_err(|e| anyhow!("op_return payload too large: {e}"))?;
    let script = bitcoin::script::Builder::new()
        .push_opcode(OP_RETURN)
        .push_slice(&data)
        .into_script();
    Ok(TxOut {
        value: Amount::ZERO,
        script_pubkey: script,
    })
}

/// Insert a plain OP_RETURN output before the anchor (last) output.
pub fn attach_op_return(ark_tx: &mut Psbt, payload: &[u8]) -> Result<()> {
    let txout = op_return_txout(payload)?;
    let len = ark_tx.unsigned_tx.output.len();
    let anchor_index = len - 1;
    ark_tx.unsigned_tx.output.insert(anchor_index, txout);
    ark_tx.outputs.insert(anchor_index, psbt::Output::default());
    Ok(())
}

fn make_sign_fn<'a>(
    keys: &'a Keys,
) -> impl FnMut(
    &mut psbt::Input,
    Message,
) -> Result<Vec<(schnorr::Signature, XOnlyPublicKey)>, ark_core::Error>
       + 'a {
    |_input, msg| Ok(keys.sign_msg(&msg))
}

/// Sign ark inputs, submit, sign returned checkpoints, finalize.
/// Returns the ark txid once preconfirmed.
pub async fn run_tx(
    keys: &Keys,
    rest: &ArkadeRest,
    ark_tx: Psbt,
    checkpoint_txs: Vec<Psbt>,
) -> Result<RunTxStatus> {
    let prepared = prepare_tx(keys, ark_tx, checkpoint_txs)?;
    submit_prepared(keys, rest, prepared.signed_ark, prepared.checkpoints).await
}

/// Sign a transaction completely on the client without contacting the
/// operator. Persist the returned value before calling submission recovery so
/// a reload can always retry the exact same transaction.
pub fn prepare_tx(
    keys: &Keys,
    mut ark_tx: Psbt,
    checkpoint_txs: Vec<Psbt>,
) -> Result<UnknownSubmission> {
    for i in 0..checkpoint_txs.len() {
        sign_ark_transaction(make_sign_fn(keys), &mut ark_tx, i)
            .map_err(|e| anyhow!("sign ark input {i}: {e}"))?;
    }
    Ok(UnknownSubmission {
        txid: ark_tx.unsigned_tx.compute_txid(),
        signed_ark: ark_tx,
        checkpoints: checkpoint_txs,
        last_error: "prepared locally; awaiting durable submission".to_string(),
    })
}

async fn submit_prepared(
    keys: &Keys,
    rest: &ArkadeRest,
    ark_tx: Psbt,
    checkpoint_txs: Vec<Psbt>,
) -> Result<RunTxStatus> {
    let expected_txid = ark_tx.unsigned_tx.compute_txid();
    let (txid, returned_ark, returned_checkpoints) =
        match rest.submit_tx(&ark_tx, &checkpoint_txs).await {
            Ok(response) => response,
            Err(error) => {
                let text = format!("{error:#}");
                if text.contains("failed (4") {
                    return Err(error);
                }
                return Ok(RunTxStatus::SubmissionUnknown(UnknownSubmission {
                    txid: expected_txid,
                    signed_ark: ark_tx,
                    checkpoints: checkpoint_txs,
                    last_error: text,
                }));
            }
        };
    let (signed_ark, signed_checkpoints) = verify_submit_response(
        keys,
        &ark_tx,
        &checkpoint_txs,
        txid,
        returned_ark,
        returned_checkpoints,
    )?;
    finalize_verified_response(keys, rest, txid, signed_ark, signed_checkpoints).await
}

async fn finalize_verified_response(
    keys: &Keys,
    rest: &ArkadeRest,
    txid: Txid,
    signed_ark: Psbt,
    signed_checkpoints: Vec<Psbt>,
) -> Result<RunTxStatus> {
    let pending = PendingFinalize {
        txid,
        signed_ark,
        checkpoints: signed_checkpoints,
        last_error: String::new(),
    };
    match finalize_pending(keys, rest, &pending).await {
        Ok(()) => Ok(RunTxStatus::Finalized(txid)),
        Err(error) => Ok(RunTxStatus::Pending(PendingFinalize {
            last_error: format!("{error:#}"),
            ..pending
        })),
    }
}

/// Recover a request whose submit response was lost. If the operator did not
/// accept it, resubmit the exact same signed transaction instead of rebuilding
/// it with a new sequence or asset burn.
pub async fn retry_unknown_submission(
    keys: &Keys,
    params: &ServerParams,
    rest: &ArkadeRest,
    unknown: &UnknownSubmission,
) -> Result<RunTxStatus> {
    let intent = pending_transaction_intent(keys, params, &unknown.checkpoints)?;
    let pending_txs = rest.get_pending_txs(intent).await?;
    if let Some((txid, returned_ark, returned_checkpoints)) = pending_txs
        .into_iter()
        .find(|(txid, _, _)| *txid == unknown.txid)
    {
        let (signed_ark, signed_checkpoints) = verify_submit_response(
            keys,
            &unknown.signed_ark,
            &unknown.checkpoints,
            txid,
            returned_ark,
            returned_checkpoints,
        )?;
        return finalize_verified_response(keys, rest, txid, signed_ark, signed_checkpoints).await;
    }

    submit_prepared(
        keys,
        rest,
        unknown.signed_ark.clone(),
        unknown.checkpoints.clone(),
    )
    .await
}

fn pending_transaction_intent(
    keys: &Keys,
    params: &ServerParams,
    checkpoints: &[Psbt],
) -> Result<ark_core::intent::Intent> {
    if checkpoints.is_empty() || checkpoints.len() > 20 {
        return Err(anyhow!(
            "pending recovery requires between 1 and 20 checkpoint inputs"
        ));
    }
    let vtxo = player_vtxo(keys, params)?;
    let mut inputs = Vec::with_capacity(checkpoints.len());
    for checkpoint in checkpoints {
        if checkpoint.unsigned_tx.input.len() != 1 || checkpoint.inputs.len() != 1 {
            return Err(anyhow!("pending checkpoint has an invalid input count"));
        }
        let psbt_input = &checkpoint.inputs[0];
        let (control_block, (spend_script, _)) = psbt_input
            .tap_scripts
            .first_key_value()
            .ok_or_else(|| anyhow!("pending checkpoint has no spend path"))?;
        let witness_utxo = psbt_input
            .witness_utxo
            .clone()
            .ok_or_else(|| anyhow!("pending checkpoint has no witness UTXO"))?;
        inputs.push(ark_core::intent::Input::new(
            checkpoint.unsigned_tx.input[0].previous_output,
            checkpoint.unsigned_tx.input[0].sequence,
            None,
            witness_utxo,
            vtxo.tapscripts().to_vec(),
            (spend_script.clone(), control_block.clone()),
            false,
            false,
            Vec::new(),
        ));
    }

    ark_core::intent::make_intent(
        |_input, message| Ok(keys.sign_msg(&message)),
        |_input, _message| Err(ark_core::Error::ad_hoc("unexpected onchain input")),
        inputs,
        Vec::new(),
        ark_core::intent::IntentMessage::GetPendingTx { expire_at: 0 },
    )
    .map_err(|error| anyhow!("build pending transaction ownership proof: {error}"))
}

fn verify_submit_response(
    keys: &Keys,
    expected_ark: &Psbt,
    expected_checkpoints: &[Psbt],
    response_txid: Txid,
    returned_ark: Psbt,
    returned_checkpoints: Vec<Psbt>,
) -> Result<(Psbt, Vec<Psbt>)> {
    let expected_txid = expected_ark.unsigned_tx.compute_txid();
    if response_txid != expected_txid
        || returned_ark.unsigned_tx.compute_txid() != expected_txid
        || returned_ark.unsigned_tx != expected_ark.unsigned_tx
    {
        return Err(anyhow!("operator changed the submitted Ark transaction"));
    }
    if returned_ark.inputs.len() != expected_ark.inputs.len() {
        return Err(anyhow!("operator returned an invalid Ark PSBT"));
    }

    let mut verified_ark = expected_ark.clone();
    for input_index in 0..expected_ark.inputs.len() {
        let (key, signature) =
            verified_server_signature(keys, expected_ark, &returned_ark, input_index)?;
        verified_ark.inputs[input_index]
            .tap_script_sigs
            .insert(key, signature);
    }

    if returned_checkpoints.len() != expected_checkpoints.len() {
        return Err(anyhow!(
            "operator returned {} checkpoints for {} inputs",
            returned_checkpoints.len(),
            expected_checkpoints.len()
        ));
    }
    let mut returned_by_txid = std::collections::HashMap::new();
    for checkpoint in returned_checkpoints {
        let txid = checkpoint.unsigned_tx.compute_txid();
        if returned_by_txid.insert(txid, checkpoint).is_some() {
            return Err(anyhow!("operator returned duplicate checkpoint {txid}"));
        }
    }

    let mut verified_checkpoints = Vec::with_capacity(expected_checkpoints.len());
    for expected in expected_checkpoints {
        let checkpoint_txid = expected.unsigned_tx.compute_txid();
        let returned = returned_by_txid
            .remove(&checkpoint_txid)
            .ok_or_else(|| anyhow!("operator omitted checkpoint {checkpoint_txid}"))?;
        if returned.unsigned_tx != expected.unsigned_tx
            || returned.inputs.len() != expected.inputs.len()
        {
            return Err(anyhow!(
                "operator changed checkpoint transaction {checkpoint_txid}"
            ));
        }
        let mut verified = expected.clone();
        for input_index in 0..expected.inputs.len() {
            let (key, signature) =
                verified_server_signature(keys, expected, &returned, input_index)?;
            verified.inputs[input_index]
                .tap_script_sigs
                .insert(key, signature);
        }
        verified_checkpoints.push(verified);
    }
    if !returned_by_txid.is_empty() {
        return Err(anyhow!("operator returned unexpected checkpoints"));
    }
    Ok((verified_ark, verified_checkpoints))
}

fn verified_server_signature(
    keys: &Keys,
    expected: &Psbt,
    returned: &Psbt,
    input_index: usize,
) -> Result<((XOnlyPublicKey, TapLeafHash), bitcoin::taproot::Signature)> {
    let expected_input = expected
        .inputs
        .get(input_index)
        .ok_or_else(|| anyhow!("missing expected PSBT input {input_index}"))?;
    let (_, (spend_script, leaf_version)) = expected_input
        .tap_scripts
        .first_key_value()
        .ok_or_else(|| anyhow!("expected PSBT input {input_index} has no spend script"))?;
    let server_keys: Vec<_> = ark_core::script::extract_checksig_pubkeys(spend_script)
        .into_iter()
        .filter(|pk| *pk != keys.owner_pk())
        .collect();
    if server_keys.len() != 1 {
        return Err(anyhow!(
            "expected PSBT input {input_index} does not have one server signer"
        ));
    }
    let server_key = server_keys[0];
    let leaf_hash = TapLeafHash::from_script(spend_script, *leaf_version);
    let signature = returned
        .inputs
        .get(input_index)
        .and_then(|input| input.tap_script_sigs.get(&(server_key, leaf_hash)))
        .cloned()
        .ok_or_else(|| anyhow!("operator signature missing from PSBT input {input_index}"))?;
    if signature.sighash_type != TapSighashType::Default {
        return Err(anyhow!(
            "operator used an unexpected sighash on PSBT input {input_index}"
        ));
    }

    let prevouts = expected
        .inputs
        .iter()
        .map(|input| {
            input
                .witness_utxo
                .clone()
                .ok_or_else(|| anyhow!("expected PSBT input is missing its witness UTXO"))
        })
        .collect::<Result<Vec<_>>>()?;
    let sighash = SighashCache::new(&expected.unsigned_tx)
        .taproot_script_spend_signature_hash(
            input_index,
            &Prevouts::All(&prevouts),
            leaf_hash,
            signature.sighash_type,
        )
        .map_err(|error| anyhow!("operator signature sighash: {error}"))?;
    let message = Message::from_digest(sighash.to_raw_hash().to_byte_array());
    keys.secp
        .verify_schnorr(&signature.signature, &message, &server_key)
        .map_err(|error| {
            anyhow!("invalid operator signature on PSBT input {input_index}: {error}")
        })?;
    Ok(((server_key, leaf_hash), signature))
}

/// Retry finalization without rebuilding or resubmitting the Ark transaction.
pub async fn finalize_pending(
    keys: &Keys,
    rest: &ArkadeRest,
    pending: &PendingFinalize,
) -> Result<()> {
    let checkpoints = sign_pending_checkpoints(keys, pending)?;
    finalize_checkpoints(rest, pending.txid, &checkpoints).await
}

fn sign_pending_checkpoints(keys: &Keys, pending: &PendingFinalize) -> Result<Vec<Psbt>> {
    let mut checkpoints = pending.checkpoints.clone();
    for checkpoint in &mut checkpoints {
        let input = checkpoint
            .inputs
            .first_mut()
            .ok_or_else(|| anyhow!("pending checkpoint has no input"))?;
        if input.witness_script.is_none() {
            return Err(anyhow!("pending checkpoint missing witness script"));
        }
        sign_checkpoint_transaction(make_sign_fn(keys), checkpoint)
            .map_err(|error| anyhow!("sign checkpoint: {error}"))?;
    }
    Ok(checkpoints)
}

async fn finalize_checkpoints(rest: &ArkadeRest, txid: Txid, checkpoints: &[Psbt]) -> Result<()> {
    let mut last_err = None;
    for attempt in 0..3 {
        if attempt > 0 {
            sleep_ms(500 * attempt as u64).await;
        }
        match rest.finalize_tx(txid, checkpoints).await {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err
        .expect("attempted")
        .context("finalize failed after retries"))
}

#[cfg(target_arch = "wasm32")]
async fn sleep_ms(ms: u64) {
    use wasm_bindgen::JsCast;
    let p = js_sys::Promise::new(&mut |resolve, _| {
        web_sys::window()
            .expect("window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                resolve.unchecked_ref(),
                ms as i32,
            )
            .expect("setTimeout");
    });
    let _ = wasm_bindgen_futures::JsFuture::from(p).await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

/// Sweep all BTC and assets to one recipient without dropping asset packets.
pub fn build_sweep_tx(
    keys: &Keys,
    params: &ServerParams,
    info: &server::Info,
    records: &[VtxoRecord],
    recipient: ark_core::ArkAddress,
) -> Result<(Psbt, Vec<Psbt>)> {
    if records.is_empty() {
        return Err(anyhow!("cannot sweep an empty wallet"));
    }
    let vtxo = player_vtxo(keys, params)?;
    let own_address = vtxo.to_ark_address();
    let inputs: Vec<_> = records
        .iter()
        .map(|record| vtxo_input(record, &vtxo))
        .collect::<Result<_>>()?;
    let amount: Amount = inputs.iter().map(|input| input.amount()).sum();
    let mut asset_totals: std::collections::HashMap<AssetId, u64> =
        std::collections::HashMap::new();
    for input in &inputs {
        for asset in input.assets() {
            let total = asset_totals.entry(asset.asset_id).or_default();
            *total = total.saturating_add(asset.amount);
        }
    }
    let receiver = ark_core::send::SendReceiver {
        address: recipient,
        amount,
        assets: asset_totals
            .into_iter()
            .map(|(asset_id, amount)| Asset { asset_id, amount })
            .collect(),
    };
    let txs = build_asset_send_transactions(&[receiver], &own_address, &inputs, info)
        .map_err(|e| anyhow!("build wallet sweep: {e}"))?;
    Ok((txs.ark_tx, txs.checkpoint_txs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::opcodes::all::OP_PUSHNUM_1;

    fn test_params(server_keypair: &bitcoin::key::Keypair) -> ServerParams {
        let server_pk = server_keypair.public_key();
        ServerParams {
            server_version: "test".to_string(),
            signer_pk: server_keypair.x_only_public_key().0,
            forfeit_pk: bitcoin::PublicKey::new(server_pk),
            network: bitcoin::Network::Regtest,
            network_name: "regtest".to_string(),
            dust_sats: 330,
            vtxo_min_sats: 330,
            unilateral_exit_delay: bitcoin::Sequence::from_height(144),
            max_op_return_outputs: 2,
            checkpoint_tapscript: bitcoin::script::Builder::new()
                .push_opcode(OP_PUSHNUM_1)
                .into_script(),
            forfeit_address: "bcrt1q8frde3yn78tl9ecgq4anlz909jh0clefhucdur"
                .parse::<bitcoin::Address<_>>()
                .unwrap()
                .require_network(bitcoin::Network::Regtest)
                .unwrap(),
        }
    }

    fn signed_submit_fixture() -> (Keys, ServerParams, Psbt, Vec<Psbt>, Psbt, Vec<Psbt>) {
        let secp = bitcoin::key::Secp256k1::new();
        let secret = bitcoin::secp256k1::SecretKey::from_slice(&[7; 32]).unwrap();
        let server_keypair = bitcoin::key::Keypair::from_secret_key(&secp, &secret);
        let server_pk = server_keypair.x_only_public_key().0;
        let params = test_params(&server_keypair);
        let info = server_info(&params);
        let keys = Keys::generate().unwrap();
        let player = player_vtxo(&keys, &params).unwrap();
        let funding = VtxoRecord {
            outpoint: bitcoin::OutPoint {
                txid: Txid::from_byte_array([42; 32]),
                vout: 0,
            },
            script: player.script_pubkey().to_hex_string(),
            amount_sats: 660,
            assets: Vec::new(),
            is_spent: false,
            is_preconfirmed: true,
            is_swept: false,
            is_unrolled: false,
            expires_at: None,
            ark_txid: None,
            spent_by: None,
        };
        let (mut expected_ark, expected_checkpoints, _) = build_move_asset_issuance_tx(
            &keys,
            &params,
            &info,
            player.to_ark_address(),
            &[funding],
        )
        .unwrap();
        for input_index in 0..expected_checkpoints.len() {
            sign_ark_transaction(make_sign_fn(&keys), &mut expected_ark, input_index).unwrap();
        }

        let mut returned_ark = expected_ark.clone();
        for input_index in 0..expected_checkpoints.len() {
            sign_ark_transaction(
                |_input, message| {
                    Ok(vec![(
                        secp.sign_schnorr_no_aux_rand(&message, &server_keypair),
                        server_pk,
                    )])
                },
                &mut returned_ark,
                input_index,
            )
            .unwrap();
        }
        let mut returned_checkpoints = expected_checkpoints.clone();
        for checkpoint in &mut returned_checkpoints {
            sign_checkpoint_transaction(
                |_input, message| {
                    Ok(vec![(
                        secp.sign_schnorr_no_aux_rand(&message, &server_keypair),
                        server_pk,
                    )])
                },
                checkpoint,
            )
            .unwrap();
        }
        (
            keys,
            params,
            expected_ark,
            expected_checkpoints,
            returned_ark,
            returned_checkpoints,
        )
    }

    #[test]
    fn verifies_and_sanitizes_submit_response() {
        let (keys, _, expected_ark, expected_checkpoints, returned_ark, returned_checkpoints) =
            signed_submit_fixture();
        let txid = expected_ark.unsigned_tx.compute_txid();
        let verified = verify_submit_response(
            &keys,
            &expected_ark,
            &expected_checkpoints,
            txid,
            returned_ark,
            returned_checkpoints,
        );
        assert!(verified.is_ok());
    }

    #[test]
    fn rejects_operator_transaction_mutation() {
        let (keys, _, expected_ark, expected_checkpoints, mut returned_ark, returned_checkpoints) =
            signed_submit_fixture();
        returned_ark.unsigned_tx.output[0].value = Amount::from_sat(329);
        let txid = expected_ark.unsigned_tx.compute_txid();
        let error = verify_submit_response(
            &keys,
            &expected_ark,
            &expected_checkpoints,
            txid,
            returned_ark,
            returned_checkpoints,
        )
        .unwrap_err();
        assert!(error.to_string().contains("changed the submitted Ark"));
    }

    #[test]
    fn builds_pending_recovery_ownership_intent() {
        let (keys, params, _, checkpoints, _, _) = signed_submit_fixture();
        let intent = pending_transaction_intent(&keys, &params, &checkpoints).unwrap();
        assert!(intent
            .serialize_message()
            .unwrap()
            .contains("get-pending-tx"));
        assert!(!intent.serialize_proof().is_empty());
    }
}
