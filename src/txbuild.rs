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
    build_asset_burn_transactions, build_offchain_transactions,
    build_self_asset_issuance_transactions, sign_ark_transaction, sign_checkpoint_transaction,
};
use ark_core::server;
use ark_core::Asset;
use ark_core::Vtxo;
use bitcoin::opcodes::all::OP_RETURN;
use bitcoin::psbt;
use bitcoin::secp256k1::{schnorr, Message};
use bitcoin::{Amount, Psbt, TxOut, Txid, XOnlyPublicKey};
use std::str::FromStr;

/// Everything the builders need from `server::Info`, reconstructed from the
/// REST `/v1/info` payload. Fields the builders never read are placeholders.
pub fn server_info(params: &ServerParams) -> server::Info {
    let signer_pk = params.signer_pk.public_key(bitcoin::secp256k1::Parity::Even);
    server::Info {
        version: String::new(),
        signer_pk,
        forfeit_pk: signer_pk,
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
    Some(ark_core::asset::AssetId { txid, group_index: u16::from_le_bytes(bytes) })
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
) -> impl FnMut(&mut psbt::Input, Message) -> Result<Vec<(schnorr::Signature, XOnlyPublicKey)>, ark_core::Error> + 'a {
    |_input, msg| Ok(keys.sign_msg(&msg))
}

/// Sign ark inputs, submit, sign returned checkpoints, finalize.
/// Returns the ark txid once preconfirmed.
pub async fn run_tx(
    keys: &Keys,
    rest: &ArkadeRest,
    mut ark_tx: Psbt,
    checkpoint_txs: Vec<Psbt>,
) -> Result<Txid> {
    for i in 0..checkpoint_txs.len() {
        sign_ark_transaction(make_sign_fn(keys), &mut ark_tx, i)
            .map_err(|e| anyhow!("sign ark input {i}: {e}"))?;
    }

    let (txid, signed_ark, signed_checkpoints) = rest.submit_tx(&ark_tx, &checkpoint_txs).await?;

    // Restore witness scripts the server may have stripped, then sign each
    // checkpoint (mirrors the SDK client's finalize path).
    let ark_input_idx_by_cp_txid: std::collections::HashMap<Txid, usize> = signed_ark
        .unsigned_tx
        .input
        .iter()
        .enumerate()
        .map(|(i, inp)| (inp.previous_output.txid, i))
        .collect();

    let mut final_checkpoints = Vec::with_capacity(signed_checkpoints.len());
    for mut cp in signed_checkpoints {
        if cp.inputs[0].witness_script.is_none() {
            let cp_txid = cp.unsigned_tx.compute_txid();
            let idx = ark_input_idx_by_cp_txid
                .get(&cp_txid)
                .ok_or_else(|| anyhow!("checkpoint txid not among ark inputs"))?;
            cp.inputs[0].witness_script = signed_ark.inputs[*idx].witness_script.clone();
        }
        sign_checkpoint_transaction(make_sign_fn(keys), &mut cp)
            .map_err(|e| anyhow!("sign checkpoint: {e}"))?;
        final_checkpoints.push(cp);
    }

    // Finalize with a few retries: the tx stays pending server-side until
    // this lands, and a transient failure here must not strand the spend.
    let mut last_err = None;
    for attempt in 0..3 {
        if attempt > 0 {
            sleep_ms(500 * attempt as u64).await;
        }
        match rest.finalize_tx(txid, &final_checkpoints).await {
            Ok(()) => return Ok(txid),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.expect("attempted").context("finalize failed after retries"))
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

/// A chained self-send carrying a plain OP_RETURN payload.
///
/// Sends `dust` back to ourselves and lets change continue the chain — but
/// when the change would be a non-zero amount below dust (invalid on the
/// server: sub-dust band is empty when vtxo_min == dust), the whole input is
/// folded into the self output instead.
pub fn build_event_tx(
    keys: &Keys,
    params: &ServerParams,
    info: &server::Info,
    input: &VtxoRecord,
    payload: &[u8],
) -> Result<(Psbt, Vec<Psbt>)> {
    let vtxo = player_vtxo(keys, params)?;
    let own_address = vtxo.to_ark_address();
    let inputs = [vtxo_input(input, &vtxo)?];
    let send_amount = match input.amount_sats.checked_sub(params.dust_sats) {
        // change would be 0..dust → fold everything into the self output
        Some(change) if change > 0 && change < params.dust_sats => input.amount_sats,
        _ => params.dust_sats,
    };
    let receivers = [ark_core::send::SendReceiver::bitcoin(
        own_address,
        Amount::from_sat(send_amount),
    )];
    let mut txs = build_offchain_transactions(&receivers, &own_address, &inputs, info)
        .map_err(|e| anyhow!("build event tx: {e}"))?;
    attach_op_return(&mut txs.ark_tx, payload)?;
    Ok((txs.ark_tx, txs.checkpoint_txs))
}

/// Send `amount` sats to `recipient` with an OP_RETURN payload. Used for the
/// START/ACK handshake messages. If the change would be a non-zero amount
/// below dust, it is folded into the recipient output (a few extra sats to
/// the opponent are cheaper than an invalid tx).
pub fn build_message_tx(
    keys: &Keys,
    params: &ServerParams,
    info: &server::Info,
    inputs: &[VtxoRecord],
    recipient: ark_core::ArkAddress,
    amount_sats: u64,
    payload: &[u8],
) -> Result<(Psbt, Vec<Psbt>)> {
    let vtxo = player_vtxo(keys, params)?;
    let own_address = vtxo.to_ark_address();
    let inputs: Vec<_> = inputs
        .iter()
        .map(|r| vtxo_input(r, &vtxo))
        .collect::<Result<_>>()?;
    let total_in: u64 = inputs.iter().map(|i| i.amount().to_sat()).sum();
    let mut amount = amount_sats;
    if let Some(change) = total_in.checked_sub(amount) {
        if change > 0 && change < params.dust_sats {
            amount = total_in; // fold the dust change into the message
        }
    }
    let receivers = [ark_core::send::SendReceiver::bitcoin(
        recipient,
        Amount::from_sat(amount),
    )];
    let mut txs = build_offchain_transactions(&receivers, &own_address, &inputs, info)
        .map_err(|e| anyhow!("build message tx: {e}"))?;
    attach_op_return(&mut txs.ark_tx, payload)?;
    Ok((txs.ark_tx, txs.checkpoint_txs))
}

/// Plain value send with no OP_RETURN (game-key funding).
pub fn build_send_tx(
    keys: &Keys,
    params: &ServerParams,
    info: &server::Info,
    inputs: &[VtxoRecord],
    recipient: ark_core::ArkAddress,
    amount_sats: u64,
) -> Result<(Psbt, Vec<Psbt>)> {
    let vtxo = player_vtxo(keys, params)?;
    let own_address = vtxo.to_ark_address();
    let inputs: Vec<_> = inputs
        .iter()
        .map(|r| vtxo_input(r, &vtxo))
        .collect::<Result<_>>()?;
    let total_in: u64 = inputs.iter().map(|i| i.amount().to_sat()).sum();
    let mut amount = amount_sats;
    if let Some(change) = total_in.checked_sub(amount) {
        if change > 0 && change < params.dust_sats {
            amount = total_in;
        }
    }
    let receivers = [ark_core::send::SendReceiver::bitcoin(
        recipient,
        Amount::from_sat(amount),
    )];
    let txs = build_offchain_transactions(&receivers, &own_address, &inputs, info)
        .map_err(|e| anyhow!("build send tx: {e}"))?;
    Ok((txs.ark_tx, txs.checkpoint_txs))
}

/// Self-issue `amount` units of a fresh asset. Returns the derived asset ID.
pub fn build_issue_tx(
    keys: &Keys,
    params: &ServerParams,
    info: &server::Info,
    input: &VtxoRecord,
    amount: u64,
    metadata: Vec<(String, String)>,
) -> Result<(Psbt, Vec<Psbt>, AssetId)> {
    let vtxo = player_vtxo(keys, params)?;
    let own_address = vtxo.to_ark_address();
    let inputs = [vtxo_input(input, &vtxo)?];
    let issued = build_self_asset_issuance_transactions(
        &own_address,
        &own_address,
        &inputs,
        info,
        amount,
        None,
        Some(metadata),
    )
    .map_err(|e| anyhow!("build issuance tx: {e}"))?;
    let asset_id = *issued
        .asset_ids
        .first()
        .ok_or_else(|| anyhow!("no asset id derived"))?;
    Ok((issued.ark_tx, issued.checkpoint_txs, asset_id))
}

/// Burn `amount` of `asset_id` (a fired bullet) with an OP_RETURN header.
pub fn build_burn_tx(
    keys: &Keys,
    params: &ServerParams,
    info: &server::Info,
    inputs: &[VtxoRecord],
    asset_id: AssetId,
    amount: u64,
    payload: &[u8],
) -> Result<(Psbt, Vec<Psbt>)> {
    let vtxo = player_vtxo(keys, params)?;
    let own_address = vtxo.to_ark_address();
    let inputs: Vec<_> = inputs
        .iter()
        .map(|r| vtxo_input(r, &vtxo))
        .collect::<Result<_>>()?;
    let mut txs = build_asset_burn_transactions(
        &own_address,
        &own_address,
        &inputs,
        info,
        asset_id,
        amount,
    )
    .map_err(|e| anyhow!("build burn tx: {e}"))?;
    attach_op_return(&mut txs.ark_tx, payload)?;
    Ok((txs.ark_tx, txs.checkpoint_txs))
}
