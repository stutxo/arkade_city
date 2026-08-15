//! Read-only live smoke test for the shared player-registry address.

use anyhow::{ensure, Result};
use arkade_city::match_::{nums_registry_address, GAME_ADDRESS};
use arkade_city::{ArkadeRest, MUTINYNET_SERVER};

#[tokio::main]
async fn main() -> Result<()> {
    let server = std::env::args()
        .nth(1)
        .unwrap_or_else(|| MUTINYNET_SERVER.to_string());
    let rest = ArkadeRest::new(&server);
    let params = rest.get_info().await?;
    let derived = nums_registry_address(&params)?.encode();
    ensure!(params.network_name == "mutinynet", "expected Mutinynet");
    ensure!(
        derived == GAME_ADDRESS,
        "hard-coded registry address drifted"
    );

    let address = ark_core::ArkAddress::decode(&derived)?;
    let script = address.to_p2tr_script_pubkey().to_hex_string();
    let page = rest.get_vtxos_page(&script, "", 10, 1).await?;

    println!("network: {:?}", params.network);
    println!("server: {server}");
    println!("registry address: {derived}");
    println!("registry script: {script}");
    println!("registrations on first page: {}", page.vtxos.len());
    println!("SMOKE OK");
    Ok(())
}
