//! Recompute the game's NUMS-owned registry address from operator params.

use anyhow::Result;
use arkade_city::match_::{nums_registry_address, GAME_ADDRESS};
use arkade_city::{ArkadeRest, MUTINYNET_SERVER};

#[tokio::main]
async fn main() -> Result<()> {
    let server = std::env::args()
        .nth(1)
        .unwrap_or_else(|| MUTINYNET_SERVER.to_string());
    let params = ArkadeRest::new(&server).get_info().await?;
    let address = nums_registry_address(&params)?;

    println!("server:  {server}");
    println!("address: {}", address.encode());
    println!("script:  {}", address.to_p2tr_script_pubkey());
    println!(
        "matches hard-coded game: {}",
        address.encode() == GAME_ADDRESS
    );
    Ok(())
}
