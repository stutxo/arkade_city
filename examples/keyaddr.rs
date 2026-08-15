use anyhow::Result;
#[tokio::main]
async fn main() -> Result<()> {
    let key = std::env::args().nth(1).unwrap();
    let keys = arkade_duel::Keys::from_hex(&key)?;
    let rest = arkade_duel::ArkadeRest::new("https://mutinynet.arkade.sh");
    let params = rest.get_info().await?;
    println!("{}", arkade_duel::my_address(&keys, &params));
    Ok(())
}
