//! Native headless Arkade City player and asset-preserving wallet sweep tool.
//!
//!   cargo run --example bot -- play
//!   cargo run --example bot -- sweep <destination_ark_address> <key_hex>...

use anyhow::{Context, Result};
use arkade_city::match_::{GameApp, Phase};
use arkade_city::{game, txbuild, ArkadeRest, Keys, ServerParams};

const SERVER: &str = "https://mutinynet.arkade.sh";
const KEY_PATH: &str = ".arkade-maze-bot-key";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("play");
    let rest = ArkadeRest::new(SERVER);
    let params = rest.get_info().await?;

    match mode {
        "play" => {
            let keys = load_or_create_key()?;
            println!("player wallet: {}", arkade_city::my_address(&keys, &params));
            play(keys, rest, params).await
        }
        "sweep" => {
            let destination = args.get(2).context("missing destination Ark address")?;
            let keys = args
                .get(3..)
                .filter(|keys| !keys.is_empty())
                .context("usage: bot sweep <destination_ark_address> <key_hex>...")?;
            sweep(keys, destination, &rest, &params).await
        }
        _ => anyhow::bail!("usage: bot [play | sweep <destination_ark_address> <key_hex>...]"),
    }
}

fn load_or_create_key() -> Result<Keys> {
    match std::fs::read_to_string(KEY_PATH) {
        Ok(hex) => Keys::from_hex(hex.trim()),
        Err(_) => {
            let keys = Keys::generate()?;
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(KEY_PATH)?;
            file.write_all(keys.secret_hex().as_bytes())?;
            Ok(keys)
        }
    }
}

async fn play(keys: Keys, rest: ArkadeRest, params: ServerParams) -> Result<()> {
    let mut app = GameApp::new(keys, rest, params)?;
    let route = maze_route();
    let mut route_index = 0usize;
    let mut seen_logs = 0usize;

    for tick in 0..1_000u32 {
        let direction = if app.phase == Phase::Playing {
            let direction = route[route_index % route.len()];
            route_index += 1;
            vec![direction]
        } else {
            Vec::new()
        };
        app.step(&direction, None).await;

        let snapshot = app.snapshot("bot");
        if tick % 10 == 0 {
            let me = snapshot.players.iter().find(|player| player.is_me);
            println!(
                "[{tick}] phase={} balance={} assets={:?} me={:?}",
                snapshot.phase,
                snapshot.balance,
                snapshot.move_balances,
                me.map(|player| (player.x, player.y, player.laps)),
            );
        }
        for line in snapshot.log.iter().skip(seen_logs) {
            println!("  {line}");
        }
        seen_logs = snapshot.log.len();
        if app.phase == Phase::OutOfMoves {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1_300)).await;
    }
    Ok(())
}

fn maze_route() -> Vec<u8> {
    let mut route = Vec::new();
    let mut add = |direction, count| route.extend(std::iter::repeat_n(direction, count));
    add(game::DIR_UP, 7);
    add(game::DIR_RIGHT, 4);
    add(game::DIR_DOWN, 14);
    add(game::DIR_RIGHT, 4);
    add(game::DIR_UP, 14);
    add(game::DIR_RIGHT, 4);
    add(game::DIR_DOWN, 14);
    add(game::DIR_RIGHT, 6);
    add(game::DIR_UP, 7);
    route
}

async fn sweep(
    key_hexes: &[String],
    destination: &str,
    rest: &ArkadeRest,
    params: &ServerParams,
) -> Result<()> {
    let destination = ark_core::ArkAddress::decode(destination)?;
    anyhow::ensure!(
        destination.server() == params.signer_pk,
        "destination belongs to a different Arkade operator"
    );
    for key_hex in key_hexes {
        let keys = Keys::from_hex(key_hex)?;
        let records = rest
            .get_vtxos(&arkade_city::my_script_hex(&keys, params), "spendableOnly")
            .await?;
        let total: u64 = records.iter().map(|record| record.amount_sats).sum();
        if total == 0 {
            println!("{}: no spendable VTXOs", &key_hex[..8]);
            continue;
        }
        let (ark_tx, checkpoints) = txbuild::build_sweep_tx(
            &keys,
            params,
            &txbuild::server_info(params),
            &records,
            destination,
        )?;
        let status = txbuild::run_tx(&keys, rest, ark_tx, checkpoints)
            .await
            .context("sweep transaction")?;
        let txid = status.txid();
        match status {
            txbuild::RunTxStatus::Finalized(_) => {}
            txbuild::RunTxStatus::Pending(pending) => {
                txbuild::finalize_pending(&keys, rest, &pending)
                    .await
                    .context("retry pending sweep finalization")?;
            }
            txbuild::RunTxStatus::SubmissionUnknown(submission) => {
                anyhow::bail!("sweep submission uncertain: {}", submission.last_error);
            }
        }
        println!("{}: sweep finalized in {txid}", &key_hex[..8]);
    }
    Ok(())
}
