use anyhow::Result;
fn main() -> Result<()> {
    for a in [
        "tark1qqcpq7yq3e8hhsx6ml3fud93m7827qggaurtzu3zwsr4a0qs0gf85djqvzdugezc0whggdlpky6vdalskvjr6gjhx52ar3qllaj6fnu5nu7s3l",
        "tark1qqcpq7yq3e8hhsx6ml3fud93m7827qggaurtzu3zwsr4a0qs0gf852le6945gmkfd7c0zm7ehkp9q59ayepp56auufk4vmnfa52xq4xp7cpjpq",
    ] {
        let addr = ark_core::ArkAddress::decode(a)?;
        println!("{} => {}", &a[a.len()-8..], addr.to_p2tr_script_pubkey().to_hex_string());
    }
    Ok(())
}
