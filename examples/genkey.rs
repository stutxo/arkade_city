use anyhow::Result;
#[tokio::main]
async fn main() -> Result<()> {
    let path = std::path::Path::new("/tmp/opencode/joiner-key");
    let keys = match std::fs::read_to_string(path) {
        Ok(hex) => arkade_city::Keys::from_hex(hex.trim())?,
        Err(_) => {
            let k = arkade_city::Keys::generate()?;
            std::fs::write(path, k.secret_hex())?;
            k
        }
    };
    let rest = arkade_city::ArkadeRest::new("https://mutinynet.arkade.sh");
    let params = rest.get_info().await?;
    println!("KEY_HEX={}", keys.secret_hex());
    println!("ADDRESS={}", arkade_city::my_address(&keys, &params));
    Ok(())
}
