//! Minimal Arkade REST client over browser `fetch`.
//!
//! Only the endpoints the game needs: server info, VTXO queries, virtual tx
//! lookup, asset info, and offchain tx submit/finalize. Wire format follows
//! the SDK's `ark-rest` crate at the pinned commit.

use anyhow::{anyhow, Context, Result};
use bitcoin::OutPoint;
use bitcoin::Transaction;
use bitcoin::XOnlyPublicKey;
use serde::Deserialize;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;

#[cfg(target_arch = "wasm32")]
macro_rules! web_log {
    ($($t:tt)*) => {
        web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&format!($($t)*)))
    };
}
#[cfg(not(target_arch = "wasm32"))]
macro_rules! web_log {
    ($($t:tt)*) => {
        eprintln!($($t)*)
    };
}

#[derive(Clone, Debug)]
pub struct ServerParams {
    pub signer_pk: XOnlyPublicKey,
    pub network: bitcoin::Network,
    pub dust_sats: u64,
    pub vtxo_min_sats: u64,
    pub unilateral_exit_delay: bitcoin::Sequence,
    pub max_op_return_outputs: i64,
    pub checkpoint_tapscript: bitcoin::ScriptBuf,
    pub forfeit_address: bitcoin::Address,
}

#[derive(Clone, Debug)]
pub struct VtxoRecord {
    pub outpoint: OutPoint,
    /// PkScript hex of the VTXO (P2TR, or OP_RETURN for sub-dust).
    pub script: String,
    pub amount_sats: u64,
    pub assets: Vec<(String, u64)>,
    pub is_spent: bool,
    pub is_preconfirmed: bool,
    /// Txid of the virtual tx that created this VTXO.
    pub ark_txid: Option<String>,
    /// Txid of the tx that spent this VTXO, if any.
    pub spent_by: Option<String>,
}

#[derive(Clone)]
pub struct ArkadeRest {
    base: String,
}

#[derive(Deserialize)]
struct InfoResponse {
    #[serde(rename = "signerPubkey")]
    signer_pubkey: String,
    network: String,
    dust: String,
    #[serde(rename = "vtxoMinAmount")]
    vtxo_min_amount: Option<String>,
    #[serde(rename = "unilateralExitDelay")]
    unilateral_exit_delay: String,
    #[serde(rename = "maxOpReturnOutputs")]
    max_op_return_outputs: Option<String>,
    #[serde(rename = "checkpointTapscript")]
    checkpoint_tapscript: String,
    #[serde(rename = "forfeitAddress")]
    forfeit_address: String,
}

#[derive(Deserialize)]
struct GetVtxosResponse {
    vtxos: Option<Vec<IndexerVtxo>>,
}

#[derive(Deserialize)]
struct IndexerVtxo {
    outpoint: Option<IndexerOutpoint>,
    script: Option<String>,
    amount: Option<String>,
    assets: Option<Vec<IndexerAsset>>,
    #[serde(rename = "isSpent")]
    is_spent: Option<bool>,
    #[serde(rename = "isPreconfirmed")]
    is_preconfirmed: Option<bool>,
    #[serde(rename = "arkTxid")]
    ark_txid: Option<String>,
    #[serde(rename = "spentBy")]
    spent_by: Option<String>,
}

#[derive(Deserialize)]
struct IndexerOutpoint {
    txid: String,
    vout: u32,
}

#[derive(Deserialize)]
struct IndexerAsset {
    #[serde(rename = "assetId")]
    asset_id: String,
    amount: Option<String>,
}

#[derive(Deserialize)]
struct GetVirtualTxsResponse {
    txs: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct AssetInfoResponse {
    #[serde(rename = "assetId")]
    pub asset_id: String,
    pub supply: Option<String>,
    pub metadata: Option<String>,
}

#[derive(serde::Serialize)]
struct SubmitTxRequest<'a> {
    #[serde(rename = "signedArkTx")]
    signed_ark_tx: &'a str,
    #[serde(rename = "checkpointTxs")]
    checkpoint_txs: Vec<String>,
}

#[derive(Deserialize)]
struct SubmitTxResponse {
    #[serde(rename = "arkTxid")]
    ark_txid: Option<String>,
    #[serde(rename = "finalArkTx")]
    final_ark_tx: Option<String>,
    #[serde(rename = "signedCheckpointTxs")]
    signed_checkpoint_txs: Option<Vec<String>>,
}

#[derive(serde::Serialize)]
struct FinalizeTxRequest<'a> {
    #[serde(rename = "arkTxid")]
    ark_txid: &'a str,
    #[serde(rename = "finalCheckpointTxs")]
    final_checkpoint_txs: Vec<String>,
}

#[cfg(target_arch = "wasm32")]
async fn fetch_text(method: &str, url: &str, body: Option<String>) -> Result<String> {
    let window = web_sys::window().ok_or_else(|| anyhow!("no window"))?;
    let init = web_sys::RequestInit::new();
    init.set_method(method);
    if let Some(body) = body {
        let headers = web_sys::Headers::new().map_err(|e| anyhow!("headers: {e:?}"))?;
        headers
            .set("content-type", "application/json")
            .map_err(|e| anyhow!("headers: {e:?}"))?;
        init.set_headers(&headers);
        init.set_body(&wasm_bindgen::JsValue::from_str(&body));
    }
    let request = web_sys::Request::new_with_str_and_init(url, &init)
        .map_err(|e| anyhow!("build request {url}: {e:?}"))?;
    let resp_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|e| anyhow!("fetch {url}: {e:?}"))?;
    let resp: web_sys::Response = resp_value
        .dyn_into()
        .map_err(|e| anyhow!("not a response: {e:?}"))?;
    let status = resp.status();
    let text = JsFuture::from(resp.text().map_err(|e| anyhow!("body: {e:?}"))?)
        .await
        .map_err(|e| anyhow!("body: {e:?}"))?
        .as_string()
        .ok_or_else(|| anyhow!("non-text body"))?;
    if !resp.ok() {
        return Err(anyhow!("{method} {url} failed ({status}): {text}"));
    }
    Ok(text)
}

/// Native transport, used by tests and tooling. Same REST wire format.
#[cfg(not(target_arch = "wasm32"))]
async fn fetch_text(method: &str, url: &str, body: Option<String>) -> Result<String> {
    let client = reqwest::Client::new();
    let req = match method {
        "POST" => client.post(url),
        _ => client.get(url),
    };
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(b),
        None => req,
    };
    let resp = client.execute(req.build()?).await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        return Err(anyhow!("{method} {url} failed ({status}): {text}"));
    }
    Ok(text)
}

/// Mirrors the SDK's `parse_sequence_number`: < 512 is blocks, >= 512 seconds.
fn parse_sequence(value: i64) -> Result<bitcoin::Sequence> {
    if value < 0 {
        return Err(anyhow!("invalid negative sequence {value}"));
    }
    if value < 512 {
        Ok(bitcoin::Sequence::from_height(value as u16))
    } else {
        bitcoin::Sequence::from_seconds_ceil(value as u32)
            .map_err(|e| anyhow!("invalid sequence {value}: {e}"))
    }
}

fn parse_asset_id(s: &str) -> Option<(bitcoin::Txid, u16)> {
    use std::str::FromStr;
    if s.len() != 68 {
        return None;
    }
    let txid = bitcoin::Txid::from_str(&s[..64]).ok()?;
    let bytes: [u8; 2] = bitcoin::hex::FromHex::from_hex(&s[64..]).ok()?;
    Some((txid, u16::from_le_bytes(bytes)))
}

impl ArkadeRest {
    pub fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
        }
    }

    pub async fn get_info(&self) -> Result<ServerParams> {
        let text = fetch_text("GET", &format!("{}/v1/info", self.base), None).await?;
        let info: InfoResponse =
            serde_json::from_str(&text).context("parse /v1/info response")?;
        let signer_pk: bitcoin::PublicKey = info
            .signer_pubkey
            .parse()
            .context("parse signer pubkey")?;
        let network = match info.network.as_str() {
            "bitcoin" | "mainnet" => bitcoin::Network::Bitcoin,
            // mutinynet is signet-based; tark HRP applies either way.
            _ => bitcoin::Network::Signet,
        };
        if network == bitcoin::Network::Bitcoin {
            web_log!("mainnet operator — real funds, alpha software");
        }
        let delay: i64 = info
            .unilateral_exit_delay
            .parse()
            .context("parse unilateral exit delay")?;
        let checkpoint_tapscript = {
            let raw: Vec<u8> = bitcoin::hex::FromHex::from_hex(&info.checkpoint_tapscript)
                .context("parse checkpoint tapscript hex")?;
            bitcoin::ScriptBuf::from_bytes(raw)
        };
        let forfeit_address = info
            .forfeit_address
            .parse::<bitcoin::Address<_>>()
            .context("parse forfeit address")?
            .require_network(network)
            .context("forfeit address network mismatch")?;
        Ok(ServerParams {
            signer_pk: signer_pk.inner.x_only_public_key().0,
            network,
            dust_sats: info.dust.parse().context("parse dust")?,
            vtxo_min_sats: info
                .vtxo_min_amount
                .as_deref()
                .unwrap_or("1")
                .parse()
                .unwrap_or(1),
            unilateral_exit_delay: parse_sequence(delay)?,
            max_op_return_outputs: info
                .max_op_return_outputs
                .as_deref()
                .unwrap_or("0")
                .parse()
                .unwrap_or(0),
            checkpoint_tapscript,
            forfeit_address,
        })
    }

    /// Query VTXOs for a script hex. `filter` is one of "spendableOnly",
    /// "spentOnly", or "" for all.
    pub async fn get_vtxos(&self, script_hex: &str, filter: &str) -> Result<Vec<VtxoRecord>> {
        let mut url = format!(
            "{}/v1/indexer/vtxos?scripts={}&page.size=500",
            self.base, script_hex
        );
        if !filter.is_empty() {
            url.push_str(&format!("&{filter}=true"));
        }
        let text = fetch_text("GET", &url, None).await?;
        let resp: GetVtxosResponse = serde_json::from_str(&text).context("parse vtxos")?;
        let mut out = Vec::new();
        for v in resp.vtxos.unwrap_or_default() {
            let (Some(outpoint), Some(script)) = (v.outpoint, v.script) else {
                continue;
            };
            let amount_sats = v
                .amount
                .as_deref()
                .unwrap_or("0")
                .parse()
                .unwrap_or_default();
            let assets = v
                .assets
                .unwrap_or_default()
                .into_iter()
                .filter(|a| parse_asset_id(&a.asset_id).is_some())
                .map(|a| {
                    (
                        a.asset_id,
                        a.amount.as_deref().unwrap_or("0").parse().unwrap_or_default(),
                    )
                })
                .collect();
            out.push(VtxoRecord {
                outpoint: OutPoint {
                    txid: outpoint.txid.parse().context("parse vtxo txid")?,
                    vout: outpoint.vout,
                },
                script,
                amount_sats,
                assets,
                is_spent: v.is_spent.unwrap_or(false),
                is_preconfirmed: v.is_preconfirmed.unwrap_or(false),
                ark_txid: v.ark_txid,
                spent_by: v.spent_by,
            });
        }
        Ok(out)
    }

    /// Fetch full virtual transactions by txid.
    ///
    /// The REST gateway encodes the gRPC `bytes` field as base64 — entries
    /// are PSBTs (`cHNidP8…`) carrying signatures/witnesses in PSBT fields.
    /// A hex raw-transaction fallback is kept for other server versions.
    pub async fn get_virtual_txs(&self, txids: &[String]) -> Result<Vec<bitcoin::Psbt>> {
        use base64::Engine;
        if txids.is_empty() {
            return Ok(vec![]);
        }
        let url = format!(
            "{}/v1/indexer/virtualTx/{}",
            self.base,
            txids.join(",")
        );
        let text = fetch_text("GET", &url, None).await?;
        let resp: GetVirtualTxsResponse =
            serde_json::from_str(&text).context("parse virtual txs")?;
        resp.txs
            .unwrap_or_default()
            .into_iter()
            .map(|entry| {
                let b64 = base64::engine::general_purpose::STANDARD;
                if let Ok(raw) = b64.decode(&entry) {
                    if let Ok(psbt) = bitcoin::Psbt::deserialize(&raw) {
                        return Ok(psbt);
                    }
                }
                // Fallback: raw transaction hex.
                let raw: Vec<u8> = bitcoin::hex::FromHex::from_hex(&entry)
                    .map_err(|e| anyhow!("virtual tx entry neither base64-PSBT nor hex: {e}"))?;
                let tx: Transaction =
                    bitcoin::consensus::encode::deserialize(&raw).context("decode virtual tx")?;
                bitcoin::Psbt::from_unsigned_tx(tx).context("wrap raw virtual tx")
            })
            .collect()
    }

    pub async fn get_asset(&self, asset_id: &str) -> Result<AssetInfoResponse> {
        let text = fetch_text(
            "GET",
            &format!("{}/v1/indexer/asset/{asset_id}", self.base),
            None,
        )
        .await?;
        serde_json::from_str(&text).context("parse asset info")
    }

    /// Submit a signed ark tx + unsigned checkpoints; returns the server's
    /// cosigned ark tx PSBT and partially-signed checkpoint PSBTs.
    pub async fn submit_tx(
        &self,
        ark_psbt: &bitcoin::Psbt,
        checkpoint_psbts: &[bitcoin::Psbt],
    ) -> Result<(bitcoin::Txid, bitcoin::Psbt, Vec<bitcoin::Psbt>)> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        let body = serde_json::to_string(&SubmitTxRequest {
            signed_ark_tx: &b64.encode(ark_psbt.serialize()),
            checkpoint_txs: checkpoint_psbts
                .iter()
                .map(|p| b64.encode(p.serialize()))
                .collect(),
        })?;
        let text = fetch_text("POST", &format!("{}/v1/tx/submit", self.base), Some(body)).await?;
        let resp: SubmitTxResponse = serde_json::from_str(&text).context("parse submit resp")?;
        let final_ark = resp
            .final_ark_tx
            .ok_or_else(|| anyhow!("submit response missing finalArkTx"))?;
        let signed_ark = bitcoin::Psbt::deserialize(
            &b64.decode(&final_ark).context("decode finalArkTx base64")?,
        )
        .context("decode finalArkTx psbt")?;
        // The ark txid is the txid of the (cosigned) ark tx itself; the
        // server's arkTxid field may be empty over REST, so compute it.
        let txid = match resp.ark_txid.as_deref().filter(|s| s.len() == 64) {
            Some(s) => s.parse().unwrap_or_else(|_| signed_ark.unsigned_tx.compute_txid()),
            None => signed_ark.unsigned_tx.compute_txid(),
        };
        let checkpoints = resp
            .signed_checkpoint_txs
            .unwrap_or_default()
            .into_iter()
            .map(|s| {
                let raw = b64.decode(&s).context("decode checkpoint base64")?;
                bitcoin::Psbt::deserialize(&raw).context("decode checkpoint psbt")
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((txid, signed_ark, checkpoints))
    }

    pub async fn finalize_tx(&self, txid: bitcoin::Txid, checkpoints: &[bitcoin::Psbt]) -> Result<()> {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        let body = serde_json::to_string(&FinalizeTxRequest {
            ark_txid: &txid.to_string(),
            final_checkpoint_txs: checkpoints
                .iter()
                .map(|p| b64.encode(p.serialize()))
                .collect(),
        })?;
        fetch_text("POST", &format!("{}/v1/tx/finalize", self.base), Some(body)).await?;
        Ok(())
    }
}
