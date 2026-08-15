//! Arkade Duel — serverless 1v1 shooter settling its input log on Arkade.
//!
//! WASM entry points. Reentrancy discipline: after `init`, the JS driver must
//! only ever call `step`, and must `await` each call before issuing the next
//! (wasm-bindgen forbids overlapping borrows of the App object). Inputs and
//! commands travel as `step` arguments; the UI state comes back as the
//! returned snapshot.

pub mod arkade;
pub mod game;
pub mod gamelog;
pub mod keys;
pub mod match_;
pub mod txbuild;

pub use arkade::{ArkadeRest, ServerParams, VtxoRecord};
pub use gamelog::{payloads_from_tx, Msg};
pub use keys::Keys;

use match_::MatchApp;
use wasm_bindgen::prelude::*;

const STORAGE_KEY: &str = "arkade-duel:key";
const STORAGE_STATE: &str = "arkade-duel:match";
/// Bumped on every deploy; shown in the UI so stale caches are obvious.
pub const VERSION: &str = "0.2.5";
/// Public Arkade mainnet operator. Override with `?server=https://…`
/// (e.g. https://mutinynet.arkade.sh for testing).
const DEFAULT_SERVER: &str = "https://arkade.computer";

#[wasm_bindgen]
pub struct App {
    inner: MatchApp,
    state_key: String,
}

fn js_err(e: anyhow::Error) -> JsValue {
    JsValue::from_str(&format!("{e:#}"))
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

impl App {
    fn persist(&self) {
        if let Some(s) = storage() {
            let state = self.inner.export_state();
            let _ = s.set_item(&self.state_key, &state.to_string());
        }
    }
}

#[wasm_bindgen]
impl App {
    /// Load or generate the browser key and fetch server parameters.
    #[wasm_bindgen(js_name = init)]
    pub async fn init(server_url: Option<String>) -> Result<App, JsValue> {
        console_error_panic_hook::set_once();
        let rest = arkade::ArkadeRest::new(server_url.as_deref().unwrap_or(DEFAULT_SERVER));
        let params = rest.get_info().await.map_err(js_err)?;
        // Keys are scoped per network so a Mutinynet key is never reused on
        // mainnet by accident.
        let storage_key = match params.network {
            bitcoin::Network::Bitcoin => format!("{STORAGE_KEY}:mainnet"),
            _ => format!("{STORAGE_KEY}:signet"),
        };
        let keys = match storage()
            .and_then(|s| s.get_item(&storage_key).ok().flatten())
            .filter(|h| h.len() == 64)
        {
            Some(hex) => Keys::from_hex(&hex).map_err(js_err)?,
            None => {
                let keys = Keys::generate().map_err(js_err)?;
                if let Some(s) = storage() {
                    let _ = s.set_item(&storage_key, &keys.secret_hex());
                }
                keys
            }
        };
        let mut inner = MatchApp::new(keys, rest, params);
        // Restore any in-progress match; the event log is re-ingested from
        // the indexer on the first step.
        let state_key = match inner.params.network {
            bitcoin::Network::Bitcoin => format!("{STORAGE_STATE}:mainnet"),
            _ => format!("{STORAGE_STATE}:signet"),
        };
        if let Some(saved) = storage().and_then(|s| s.get_item(&state_key).ok().flatten()) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&saved) {
                inner.import_state(&v);
            }
        }
        Ok(App { inner, state_key })
    }

    #[wasm_bindgen(js_name = address)]
    pub fn address(&self) -> String {
        self.inner.my_address().encode()
    }

    /// "mainnet" or "signet" — known right after init, before any snapshot.
    #[wasm_bindgen(js_name = network)]
    pub fn network(&self) -> String {
        match self.inner.params.network {
            bitcoin::Network::Bitcoin => "mainnet".to_string(),
            _ => "signet".to_string(),
        }
    }

    /// Recovery export: the raw key hex. Anyone holding it can spend.
    #[wasm_bindgen(js_name = exportKey)]
    pub fn export_key(&self) -> String {
        self.inner.keys.secret_hex()
    }

    /// The single serialized entry point.
    ///
    /// * `command`: "", "host", "join" (`arg` = host address), or "reset"
    /// * `mask`: current WASD key bitmask (see `keyMask`)
    /// * `fires`: number of fire presses since the previous step
    ///
    /// Returns the JSON snapshot for rendering.
    #[wasm_bindgen(js_name = step)]
    pub async fn step(
        &mut self,
        command: &str,
        arg: &str,
        mask: u8,
        fires: u32,
    ) -> Result<JsValue, JsValue> {
        if command == "reset" {
            if let Some(s) = storage() {
                let _ = s.remove_item(&self.state_key);
            }
            self.inner.reset_match();
        } else {
            self.inner.handle_command(command, arg).await;
            // Persist immediately after state-changing commands: a reload
            // during the following poll must not lose a sent START.
            self.persist();
            self.inner.step(mask, fires).await.map_err(js_err)?;
        }
        self.persist();
        serde_wasm_bindgen::to_value(&self.inner.snapshot(VERSION))
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

#[wasm_bindgen(js_name = keyMask)]
pub fn key_mask(w: bool, a: bool, s: bool, d: bool) -> u8 {
    (if w { game::KEY_W } else { 0 })
        | (if a { game::KEY_A } else { 0 })
        | (if s { game::KEY_S } else { 0 })
        | (if d { game::KEY_D } else { 0 })
}

/// Convenience for examples/tools: this player's Arkade address.
pub fn my_address(keys: &Keys, params: &ServerParams) -> String {
    txbuild::player_vtxo(keys, params)
        .expect("vtxo")
        .to_ark_address()
        .encode()
}

/// Convenience for examples/tools: this player's VTXO script hex.
pub fn my_script_hex(keys: &Keys, params: &ServerParams) -> String {
    use bitcoin::hex::DisplayHex;
    txbuild::player_vtxo(keys, params)
        .expect("vtxo")
        .script_pubkey()
        .to_hex_string()
}

/// Onchain boarding address: send coins here to fund the wallet (requires a
/// full wallet to complete the board; not implemented in this client).
pub fn boarding_address(keys: &Keys, params: &ServerParams) -> anyhow::Result<String> {
    let boarding = ark_core::BoardingOutput::new(
        &keys.secp,
        params.signer_pk,
        keys.owner_pk(),
        params.unilateral_exit_delay,
        params.network,
    )
    .map_err(|e| anyhow::anyhow!("boarding output: {e}"))?;
    Ok(boarding.address().to_string())
}

/// Test helper: a minimal valid move message payload.
pub fn build_test_payload(
    _keys: &Keys,
    _params: &ServerParams,
    _input: &VtxoRecord,
) -> anyhow::Result<Vec<u8>> {
    Ok(Msg {
        match_tag: [7; 8],
        seq: 0,
        prev: [0; 8],
        tick_ms: match_::now_ms(),
        kind: gamelog::Kind::Move,
        data: vec![game::KEY_D],
    }
    .encode())
}

/// Test helper: send one chained event tx with a move payload.
pub async fn send_test_event(
    keys: &Keys,
    rest: &ArkadeRest,
    params: &ServerParams,
    input: &VtxoRecord,
) -> anyhow::Result<bitcoin::Txid> {
    let info = txbuild::server_info(params);
    let payload = build_test_payload(keys, params, input)?;
    let (ark_tx, checkpoints) = txbuild::build_event_tx(keys, params, &info, input, &payload)?;
    txbuild::run_tx(keys, rest, ark_tx, checkpoints).await
}

/// Test helper: OP_RETURN payloads of a virtual tx (PSBT from the indexer).
pub fn payloads_of(psbt: &bitcoin::Psbt) -> Vec<Vec<u8>> {
    payloads_from_tx(&psbt.unsigned_tx)
}
