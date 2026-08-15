//! Build synthetic registration and protocol-burn transactions and print them.

use anyhow::Result;
use arkade_city::match_::GAME_ADDRESS;
use arkade_city::{gamelog, txbuild, ArkadeRest, Keys, VtxoRecord, MUTINYNET_SERVER};

#[tokio::main]
async fn main() -> Result<()> {
    let rest = ArkadeRest::new(MUTINYNET_SERVER);
    let params = rest.get_info().await?;
    let keys = Keys::generate()?;
    let player = txbuild::player_vtxo(&keys, &params)?;
    let script = player.script_pubkey().to_hex_string();
    let info = txbuild::server_info(&params);
    let game = ark_core::ArkAddress::decode(GAME_ADDRESS)?;
    let game_script = game.to_p2tr_script_pubkey().to_hex_string();
    let funding = VtxoRecord {
        outpoint: bitcoin::OutPoint {
            txid: "aa".repeat(32).parse()?,
            vout: 0,
        },
        script: script.clone(),
        amount_sats: params.dust_sats * 2,
        assets: Vec::new(),
        is_spent: false,
        is_preconfirmed: true,
        is_swept: false,
        is_unrolled: false,
        expires_at: None,
        ark_txid: None,
        spent_by: None,
    };

    let (issuance, _, assets) =
        txbuild::build_move_asset_issuance_tx(&keys, &params, &info, game, &[funding])?;
    assert_eq!(
        gamelog::registration_player_script(&issuance.unsigned_tx, &game_script, params.dust_sats,),
        Some(script.clone())
    );
    print_outputs("REGISTER + ISSUE 50 W/A/S/D", &issuance);

    let carrier = VtxoRecord {
        outpoint: bitcoin::OutPoint {
            txid: issuance.unsigned_tx.compute_txid(),
            vout: 1,
        },
        script,
        amount_sats: params.dust_sats,
        assets: assets
            .iter()
            .map(|asset| (asset.to_string(), txbuild::MOVE_SUPPLY))
            .collect(),
        is_spent: false,
        is_preconfirmed: true,
        is_swept: false,
        is_unrolled: false,
        expires_at: None,
        ark_txid: None,
        spent_by: None,
    };
    let (movement, _) = txbuild::build_move_burn_tx(&keys, &params, &info, &carrier, assets[0], 0)?;
    let burn = gamelog::move_burn_from_tx(&movement.unsigned_tx).expect("move burn");
    assert_eq!(burn.asset_id, assets[0]);
    assert_eq!(burn.receipt.sequence, 0);
    assert_eq!(burn.preserved_output_indexes, [0]);
    print_outputs("BURN W + RECREATE CARRIER", &movement);
    Ok(())
}

fn print_outputs(label: &str, psbt: &bitcoin::Psbt) {
    println!("{label}");
    for (index, output) in psbt.unsigned_tx.output.iter().enumerate() {
        println!(
            "  #{index} value={} script={}",
            output.value.to_sat(),
            output.script_pubkey.to_hex_string()
        );
    }
}
