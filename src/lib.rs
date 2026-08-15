//! Arkade City: one player registry and indexable asset-burn move chains.

pub mod arkade;
pub mod game;
pub mod gamelog;
pub mod keys;
pub mod match_;
pub mod txbuild;

pub use arkade::{ArkadeRest, ServerParams, VtxoPage, VtxoRecord};
pub use keys::Keys;

use match_::GameApp;
use wasm_bindgen::prelude::*;

pub const VERSION: &str = "2.3.1";
pub const MUTINYNET_SERVER: &str = "https://mutinynet.arkade.sh";

fn js_err(error: anyhow::Error) -> JsValue {
    JsValue::from_str(&format!("{error:#}"))
}

#[wasm_bindgen]
pub struct App {
    inner: GameApp,
}

#[wasm_bindgen]
impl App {
    /// Connect to one Arkade server and restore the supplied wallet key. The
    /// browser owns persistence so a failed network request cannot replace it.
    #[wasm_bindgen(js_name = init)]
    pub async fn init(
        server: String,
        secret_key: Option<String>,
        pending_journal: Option<String>,
    ) -> Result<App, JsValue> {
        console_error_panic_hook::set_once();
        if server.trim_end_matches('/') != MUTINYNET_SERVER {
            return Err(JsValue::from_str(
                "Arkade City currently supports Mutinynet only",
            ));
        }
        let rest = ArkadeRest::new(MUTINYNET_SERVER);
        let params = rest.get_info().await.map_err(js_err)?;
        let keys = match secret_key.filter(|value| !value.trim().is_empty()) {
            Some(secret) => Keys::from_hex(secret.trim()).map_err(js_err)?,
            None => Keys::generate().map_err(js_err)?,
        };
        let mut inner = GameApp::new(keys, rest, params).map_err(js_err)?;
        if let Some(journal) = pending_journal.filter(|value| !value.trim().is_empty()) {
            if let Err(error) = inner.restore_pending_journal(&journal) {
                inner.log.push(format!(
                    "ignored invalid pending transaction journal: {error:#}"
                ));
            }
        }
        Ok(Self { inner })
    }

    #[wasm_bindgen(js_name = address)]
    pub fn address(&self) -> String {
        self.inner.player_address().encode()
    }

    #[wasm_bindgen(js_name = gameAddress)]
    pub fn game_address(&self) -> String {
        self.inner.game_address()
    }

    #[wasm_bindgen(js_name = exportKey)]
    pub fn export_key(&self) -> String {
        self.inner.keys.secret_hex()
    }

    #[wasm_bindgen(js_name = exportPending)]
    pub fn export_pending(&self) -> String {
        self.inner
            .export_pending_journal()
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Recovery data includes the contract parameters needed to rediscover
    /// this tab's VTXOs after an operator signer or exit-delay rotation.
    #[wasm_bindgen(js_name = exportRecovery)]
    pub fn export_recovery(&self) -> String {
        let pending = self
            .inner
            .export_pending_journal()
            .ok()
            .flatten()
            .and_then(|journal| serde_json::from_str::<serde_json::Value>(&journal).ok());
        serde_json::json!({
            "secretKey": self.inner.keys.secret_hex(),
            "address": self.inner.player_address().encode(),
            "server": self.inner.rest.base(),
            "serverSigner": self.inner.params.signer_pk.to_string(),
            "exitDelay": self.inner.params.unilateral_exit_delay.to_consensus_u32(),
            "network": self.inner.params.network_name,
            "registry": self.inner.game_address(),
            "pending": pending,
        })
        .to_string()
    }

    /// Return local wallet/game state immediately, without waiting on the
    /// indexer. The browser uses this to render the restored wallet while the
    /// first network synchronization runs.
    #[wasm_bindgen(js_name = snapshot)]
    pub fn snapshot(&self) -> Result<JsValue, JsValue> {
        serde_wasm_bindgen::to_value(&self.inner.snapshot(VERSION))
            .map_err(|error| JsValue::from_str(&error.to_string()))
    }

    /// Synchronize state and execute at most one requested wallet/game action.
    #[wasm_bindgen(js_name = step)]
    pub async fn step(
        &mut self,
        dirs: Vec<u8>,
        enter_game: bool,
        sweep_address: Option<String>,
    ) -> Result<JsValue, JsValue> {
        self.inner
            .step(&dirs, enter_game, sweep_address.as_deref())
            .await;
        self.snapshot()
    }
}

/// Convenience for native examples and tools.
pub fn my_address(keys: &Keys, params: &ServerParams) -> String {
    txbuild::player_vtxo(keys, params)
        .expect("vtxo")
        .to_ark_address()
        .encode()
}

pub fn my_script_hex(keys: &Keys, params: &ServerParams) -> String {
    txbuild::player_vtxo(keys, params)
        .expect("vtxo")
        .script_pubkey()
        .to_hex_string()
}
