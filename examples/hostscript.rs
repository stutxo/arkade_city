use anyhow::Result;
fn main() -> Result<()> {
    let a = std::env::args().nth(1).unwrap();
    let addr = ark_core::ArkAddress::decode(&a)?;
    println!("{}", addr.to_p2tr_script_pubkey().to_hex_string());
    Ok(())
}
