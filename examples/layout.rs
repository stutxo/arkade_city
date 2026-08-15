// offline: build event/burn/issuance txs with a synthetic vtxo and print
// the exact output layouts the server will validate.
use anyhow::Result;
use arkade_duel::txbuild;
use arkade_duel::*;
use bitcoin::hex::DisplayHex;

#[tokio::main]
async fn main() -> Result<()> {
    let rest = ArkadeRest::new("https://mutinynet.arkade.sh");
    let params = rest.get_info().await?;
    let keys = Keys::generate()?;
    let vtxo = txbuild::player_vtxo(&keys, &params)?;
    let script_hex = vtxo.script_pubkey().to_hex_string();

    let record = |sats: u64, assets: Vec<(String, u64)>| VtxoRecord {
        outpoint: bitcoin::OutPoint {
            txid: "aa".repeat(32).parse().unwrap(),
            vout: 0,
        },
        script: script_hex.clone(),
        amount_sats: sats,
        assets,
        is_spent: false,
        is_preconfirmed: true,
        ark_txid: None,
        spent_by: None,
    };
    let info = txbuild::server_info(&params);
    let payload = Msg {
        match_tag: [1; 8],
        seq: 0,
        prev: [0; 8],
        tick_ms: 1,
        kind: gamelog::Kind::Move,
        data: vec![5],
    }
    .encode();

    for sats in [350u64, 2000, 5000] {
        let (ark_tx, _) = txbuild::build_event_tx(&keys, &params, &info, &record(sats, vec![]), &payload)?;
        println!("EVENT input={} sats:", sats);
        for (i, o) in ark_tx.unsigned_tx.output.iter().enumerate() {
            println!(
                "  #{i} value={} script={}",
                o.value.to_sat(),
                o.script_pubkey.to_hex_string()
            );
        }
    }

    // burn: carrier holding 20 units of a fake asset
    let fake_asset = format!("{}{}", "bb".repeat(32), "0000");
    let carrier = record(330, vec![(fake_asset.clone(), 20)]);
    let asset_id = txbuild::parse_asset_id_pub(&fake_asset).unwrap();
    let (burn_tx, _) = txbuild::build_burn_tx(
        &keys,
        &params,
        &info,
        &[carrier],
        asset_id,
        1,
        &payload,
    )?;
    println!("BURN carrier=330 asset=20->19:");
    for (i, o) in burn_tx.unsigned_tx.output.iter().enumerate() {
        println!(
            "  #{i} value={} script={}",
            o.value.to_sat(),
            o.script_pubkey.to_hex_string()
        );
    }
    Ok(())
}
