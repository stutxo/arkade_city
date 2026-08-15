//! Live smoke test against an Arkade operator.
//! Run: `cargo run --example smoke [-- [server_url] [key_hex]]`
//!
//! With no args it uses the persisted `.arkade-smoke-key` identity on
//! Mutinynet. Pass a server URL and a key hex (e.g. the recovery key shown
//! in the browser page) to exercise a real send with full error bodies.

use anyhow::Result;
use arkade_duel::*;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let base = args
        .get(1)
        .map(String::as_str)
        .unwrap_or("https://mutinynet.arkade.sh");

    let rest = ArkadeRest::new(base);
    let params = rest.get_info().await?;
    println!(
        "== server ==\n  signer={} net={} dust={} vtxo_min={} max_op_return={}",
        params.signer_pk, params.network, params.dust_sats, params.vtxo_min_sats,
        params.max_op_return_outputs
    );

    let keys = if let Some(hex) = args.get(2) {
        Keys::from_hex(hex.trim())?
    } else {
        let key_path = std::path::Path::new(".arkade-smoke-key");
        match std::fs::read_to_string(key_path) {
            Ok(hex) => Keys::from_hex(hex.trim())?,
            Err(_) => {
                let keys = Keys::generate()?;
                std::fs::write(key_path, keys.secret_hex())?;
                keys
            }
        }
    };

    let addr = my_address(&keys, &params);
    let script = my_script_hex(&keys, &params);
    println!("== identity ==\n  address={addr}\n  script={script}");

    let all = rest.get_vtxos(&script, "").await?;
    let spendable = rest.get_vtxos(&script, "spendableOnly").await?;
    println!(
        "== wallet ==\n  vtxos: {} total, {} spendable",
        all.len(),
        spendable.len()
    );
    for v in &all {
        println!(
            "  {}:{} amt={} spent={} preconfirmed={} arkTxid={:?} spentBy={:?}",
            v.outpoint.txid, v.outpoint.vout, v.amount_sats, v.is_spent, v.is_preconfirmed,
            v.ark_txid, v.spent_by
        );
    }
    if spendable.is_empty() {
        println!("  (unfunded — send an offchain payment to the address above)");
        return Ok(());
    }

    // Minimal event tx: self-send dust + one OP_RETURN game message.
    let input = spendable.iter().max_by_key(|v| v.amount_sats).cloned().unwrap();
    println!("== sending event tx with input {}:{} ==", input.outpoint.txid, input.outpoint.vout);
    match send_test_event(&keys, &rest, &params, &input).await {
        Ok(txid) => {
            println!("  PRECONFIRMED txid={txid}");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let txs = rest.get_virtual_txs(&[txid.to_string()]).await?;
            let payloads: Vec<_> = txs.iter().flat_map(payloads_of).collect();
            println!("  readback payloads: {} (expect 1)", payloads.len());
            for p in &payloads {
                println!("    {:?}", Msg::decode(p).map(|m| (m.kind, m.seq)));
            }
            println!("SMOKE OK");
        }
        Err(e) => {
            println!("  SEND FAILED:\n{e:#}");
            std::process::exit(1);
        }
    }
    Ok(())
}
