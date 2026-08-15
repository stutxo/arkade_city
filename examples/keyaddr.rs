use anyhow::Result;
#[tokio::main]
async fn main() -> Result<()> {
    let key = std::env::args().nth(1).unwrap();
    let keys = arkade_city::Keys::from_hex(&key)?;
    let rest = arkade_city::ArkadeRest::new("https://mutinynet.arkade.sh");
    let params = rest.get_info().await?;
    println!("{}", arkade_city::my_address(&keys, &params));
    Ok(())
}
