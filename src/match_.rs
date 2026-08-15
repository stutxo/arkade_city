//! Registry-and-burn-chain game orchestration.
//!
//! There is no lobby, match, opponent connection, or peer transport. Every
//! browser discovers players from one operator-specific registry script, then reads
//! native move-asset burns from those players' scripts through the existing
//! Arkade indexer.

use crate::arkade::{ArkadeRest, ServerParams, VtxoRecord};
use crate::game::PlayerState;
use crate::gamelog::{canonical_moves, move_burn_from_tx, registration_player_script, MoveEvent};
use crate::keys::Keys;
use crate::txbuild;
use anyhow::{anyhow, Context, Result};
use ark_core::asset::AssetId;
use base64::Engine;
use bitcoin::{OutPoint, Psbt, Txid};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

/// Mutinynet Arkade default VTXO whose owner is the standard BIP341 NUMS key.
/// Both virtual spending paths require the unknown NUMS secret. Each player
/// sends one registration output here; moves return their carrier to the
/// player's own script. The operator may sweep registration backing sats after
/// VTXO expiry.
/// Recompute it with `cargo run --example burnaddr`.
pub const GAME_ADDRESS: &str = "tark1qqcpq7yq3e8hhsx6ml3fud93m7827qggaurtzu3zwsr4a0qs0gf85xacghv2fqfrv43e4mgekvqq6ul7u65wm8u7tk3t67kl9syxt822nw9wzn";

const MAX_INPUT_QUEUE: usize = 16;
const INDEX_PAGE_SIZE: i32 = 500;
const PLAYER_SCRIPT_BATCH_SIZE: usize = 20;
const MIN_INPUT_LIFETIME_SECS: i64 = 3_600;
const PENDING_JOURNAL_VERSION: u8 = 1;
const MAX_PENDING_JOURNAL_BYTES: usize = 2_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    FundWallet,
    Issuing,
    Syncing,
    Playing,
    OutOfMoves,
}

#[derive(Clone)]
enum PendingAction {
    Issuance {
        pending: txbuild::PendingFinalize,
        asset_ids: [AssetId; 4],
    },
    Move {
        pending: txbuild::PendingFinalize,
        direction: u8,
        sequence: u32,
    },
    UnknownIssuance {
        submission: txbuild::UnknownSubmission,
        asset_ids: [AssetId; 4],
    },
    UnknownMove {
        submission: txbuild::UnknownSubmission,
        direction: u8,
        sequence: u32,
    },
    Sweep {
        pending: txbuild::PendingFinalize,
    },
    UnknownSweep {
        submission: txbuild::UnknownSubmission,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingJournal {
    version: u8,
    server: String,
    address: String,
    signer: String,
    action: PendingJournalAction,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum PendingJournalAction {
    Issuance {
        transaction: JournalTransaction,
        asset_ids: [String; 4],
    },
    Move {
        transaction: JournalTransaction,
        direction: u8,
        sequence: u32,
    },
    Sweep {
        transaction: JournalTransaction,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct JournalTransaction {
    stage: JournalStage,
    txid: String,
    signed_ark: String,
    checkpoints: Vec<String>,
    last_error: String,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum JournalStage {
    Prepared,
    Finalizing,
}

enum RestoredTransaction {
    Prepared(txbuild::UnknownSubmission),
    Finalizing(txbuild::PendingFinalize),
}

pub struct GameApp {
    pub keys: Keys,
    pub rest: ArkadeRest,
    pub params: ServerParams,
    pub info: ark_core::server::Info,
    game_address: ark_core::ArkAddress,
    game_script: String,

    pub phase: Phase,
    move_assets: Option<[AssetId; 4]>,
    move_balances: [u64; 4],
    issuance_visible: bool,
    funding_ready: bool,
    next_sequence: u32,
    pending_dirs: VecDeque<u8>,
    pub sending: bool,
    pending_action: Option<PendingAction>,

    events: Vec<MoveEvent>,
    registered_players: BTreeMap<Txid, String>,
    invalid_registrations: HashSet<Txid>,
    seen_registry_outputs: HashSet<OutPoint>,
    unresolved_registry_outputs: HashMap<OutPoint, VtxoRecord>,
    seen_player_outputs: HashSet<OutPoint>,
    unresolved_player_outputs: HashMap<OutPoint, VtxoRecord>,
    seen_move_txs: HashSet<Txid>,
    fully_scanned_player_scripts: HashSet<String>,
    initial_registry_sync: bool,
    initial_player_sync: bool,
    game_sync_count: u32,

    excluded_outpoints: HashSet<String>,
    balance_cache: u64,
    wallet_records: Vec<VtxoRecord>,
    wallet_sync_ms: u64,
    last_sync_ms: u64,
    last_error: Option<String>,
    pub log: Vec<String>,
}

impl GameApp {
    pub fn new(keys: Keys, rest: ArkadeRest, params: ServerParams) -> Result<Self> {
        if params.max_op_return_outputs < 2 || params.vtxo_min_sats > params.dust_sats {
            return Err(anyhow!(
                "operator limits no longer support one asset packet plus one move receipt"
            ));
        }
        if params.network_name != "mutinynet" {
            return Err(anyhow!("Arkade City currently supports Mutinynet only"));
        }
        let game_address = nums_registry_address(&params)?;
        if game_address.encode() != GAME_ADDRESS {
            return Err(anyhow!(
                "Mutinynet operator parameters do not match the pinned game registry"
            ));
        }
        let game_script = game_address.to_p2tr_script_pubkey().to_hex_string();
        let info = txbuild::server_info(&params);
        let network_name = params.network_name.clone();
        let server = rest.base().to_string();
        Ok(Self {
            keys,
            rest,
            params,
            info,
            game_address,
            game_script,
            phase: Phase::FundWallet,
            move_assets: None,
            move_balances: [0; 4],
            issuance_visible: false,
            funding_ready: false,
            next_sequence: 0,
            pending_dirs: VecDeque::new(),
            sending: false,
            pending_action: None,
            events: Vec::new(),
            registered_players: BTreeMap::new(),
            invalid_registrations: HashSet::new(),
            seen_registry_outputs: HashSet::new(),
            unresolved_registry_outputs: HashMap::new(),
            seen_player_outputs: HashSet::new(),
            unresolved_player_outputs: HashMap::new(),
            seen_move_txs: HashSet::new(),
            fully_scanned_player_scripts: HashSet::new(),
            initial_registry_sync: false,
            initial_player_sync: false,
            game_sync_count: 0,
            excluded_outpoints: HashSet::new(),
            balance_cache: 0,
            wallet_records: Vec::new(),
            wallet_sync_ms: 0,
            last_sync_ms: 0,
            last_error: None,
            log: vec![format!(
                "connected to {network_name} at {server}; watching registry {}",
                game_address.encode()
            )],
        })
    }

    pub fn player_address(&self) -> ark_core::ArkAddress {
        txbuild::player_vtxo(&self.keys, &self.params)
            .expect("player VTXO")
            .to_ark_address()
    }

    fn player_script(&self) -> String {
        txbuild::player_vtxo(&self.keys, &self.params)
            .expect("player VTXO")
            .script_pubkey()
            .to_hex_string()
    }

    pub fn game_address(&self) -> String {
        self.game_address.encode()
    }

    pub fn export_pending_journal(&self) -> Result<Option<String>> {
        let Some(action) = self.pending_action.as_ref() else {
            return Ok(None);
        };
        let journal = PendingJournal {
            version: PENDING_JOURNAL_VERSION,
            server: self.rest.base().to_string(),
            address: self.player_address().encode(),
            signer: self.params.signer_pk.to_string(),
            action: pending_action_to_journal(action),
        };
        Ok(Some(serde_json::to_string(&journal)?))
    }

    pub fn restore_pending_journal(&mut self, raw: &str) -> Result<()> {
        if raw.len() > MAX_PENDING_JOURNAL_BYTES {
            return Err(anyhow!("pending transaction journal is too large"));
        }
        let journal: PendingJournal =
            serde_json::from_str(raw).context("parse pending transaction journal")?;
        if journal.version != PENDING_JOURNAL_VERSION {
            return Err(anyhow!(
                "unsupported pending transaction journal version {}",
                journal.version
            ));
        }
        if journal.server != self.rest.base() {
            return Err(anyhow!("pending transaction belongs to a different server"));
        }
        if journal.address != self.player_address().encode() {
            return Err(anyhow!("pending transaction belongs to a different wallet"));
        }
        if journal.signer != self.params.signer_pk.to_string() {
            return Err(anyhow!(
                "pending transaction belongs to a different operator signer"
            ));
        }

        let action = pending_action_from_journal(journal.action)?;
        let txid = pending_action_txid(&action);
        if let PendingAction::Issuance { asset_ids, .. }
        | PendingAction::UnknownIssuance { asset_ids, .. } = &action
        {
            self.phase = Phase::Issuing;
            self.move_assets = Some(*asset_ids);
            self.move_balances = [txbuild::MOVE_SUPPLY; 4];
            self.next_sequence = 0;
            self.registered_players.insert(txid, self.player_script());
        }
        self.pending_action = Some(action);
        self.log_line(format!(
            "restored pending transaction {} from browser journal",
            short_txid(&txid)
        ));
        Ok(())
    }

    pub fn required_funding(&self) -> u64 {
        self.params.dust_sats.saturating_mul(2)
    }

    fn log_line(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() > 100 {
            self.log.remove(0);
        }
    }

    pub fn queue_inputs(&mut self, dirs: &[u8]) {
        if self.phase != Phase::Playing || self.pending_action.is_some() {
            return;
        }
        for &dir in dirs {
            if dir <= crate::game::DIR_LEFT && self.pending_dirs.len() < MAX_INPUT_QUEUE {
                self.pending_dirs.push_back(dir);
            }
        }
    }

    /// Serialized browser tick: refresh wallet state, recover pending wallet
    /// actions, then synchronize game history before executing a move.
    pub async fn step(&mut self, dirs: &[u8], enter_game: bool, sweep_address: Option<&str>) {
        self.queue_inputs(dirs);

        let player_script = self.player_script();
        let spendable = match self.rest.get_vtxos(&player_script, "spendableOnly").await {
            Ok(records) => records,
            Err(err) => {
                self.report_error("wallet sync", &err);
                return;
            }
        };
        // The indexer extracts the P2TR pubkey and returns regular and sub-dust
        // VTXOs associated with that key.
        let diagnostics_ready = match self.rest.get_vtxos(&player_script, "").await {
            Ok(records) => {
                self.wallet_records = records;
                self.wallet_sync_ms = now_ms();
                true
            }
            Err(err) => {
                self.report_error("wallet diagnostics", &err);
                false
            }
        };
        if self.move_assets.is_none() {
            match self.discover_move_assets(&spendable).await {
                Ok(()) => {}
                Err(err) => {
                    self.report_error("asset recovery", &err);
                    return;
                }
            }
        }
        self.refresh_wallet_state(&spendable);
        self.reconcile_next_sequence();

        if self.pending_action.is_some() {
            self.resume_pending(&spendable).await;
            return;
        }

        if let Some(destination) = sweep_address {
            if let Err(err) = self.sweep_wallet(&spendable, destination).await {
                self.handle_send_error("wallet sweep", &err);
            }
            return;
        }

        let game_ready = match self.refresh_game().await {
            Ok(()) => true,
            Err(err) => {
                self.report_error("game sync", &err);
                false
            }
        };
        self.reconcile_next_sequence();

        if !game_ready {
            return;
        }
        if diagnostics_ready {
            self.clear_error();
        }

        if self.move_assets.is_none() {
            if enter_game && self.funding_ready && !self.sending {
                if let Err(err) = self.issue_move_assets(&spendable).await {
                    self.handle_send_error("issuance", &err);
                }
            }
            return;
        }

        if self.issuance_visible && self.move_balances.iter().all(|amount| *amount == 0) {
            self.phase = Phase::OutOfMoves;
            self.pending_dirs.clear();
            return;
        }
        if self.issuance_visible {
            if !self.local_history_ready() {
                self.phase = Phase::Syncing;
                return;
            }
            self.phase = Phase::Playing;
        } else {
            self.phase = Phase::Issuing;
            return;
        }

        if !self.sending {
            self.flush_one_move(&spendable).await;
        }
    }

    fn refresh_wallet_state(&mut self, spendable: &[VtxoRecord]) {
        self.balance_cache = spendable.iter().map(|record| record.amount_sats).sum();
        self.funding_ready = self.registration_inputs(spendable).is_some();
        let Some(asset_ids) = self.move_assets else {
            self.phase = Phase::FundWallet;
            return;
        };

        let mut balances = [0u64; 4];
        for record in spendable {
            for (id, amount) in &record.assets {
                for (direction, asset_id) in asset_ids.iter().enumerate() {
                    if id == &asset_id.to_string() {
                        balances[direction] = balances[direction].saturating_add(*amount);
                        self.issuance_visible = true;
                    }
                }
            }
        }
        self.move_balances = balances;
    }

    async fn discover_move_assets(&mut self, spendable: &[VtxoRecord]) -> Result<()> {
        let mut candidates = HashSet::new();
        for record in spendable {
            for (id, amount) in &record.assets {
                let Some(asset_id) = txbuild::parse_asset_id_pub(id) else {
                    continue;
                };
                if *amount > 0 && asset_id.group_index < 4 {
                    candidates.insert(asset_id.txid);
                }
            }
        }
        let own_script = self.player_script();
        for txid in candidates {
            if self.issuance_player_script(txid).await?.as_deref() == Some(own_script.as_str()) {
                let asset_ids = std::array::from_fn(|group_index| AssetId {
                    txid,
                    group_index: group_index as u16,
                });
                self.move_assets = Some(asset_ids);
                self.log_line(format!(
                    "recovered move asset issuance {}",
                    short_txid(&txid)
                ));
                break;
            }
        }
        Ok(())
    }

    fn reconcile_next_sequence(&mut self) {
        let Some(player) = self.player_id() else {
            return;
        };
        let contiguous = canonical_moves(&self.events)
            .get(&player)
            .map_or(0, |moves| moves.len() as u32);
        self.next_sequence = self.next_sequence.max(contiguous);
    }

    fn local_history_ready(&self) -> bool {
        let Some(player) = self.player_id() else {
            return false;
        };
        let Some(script) = self.registered_players.get(&player) else {
            return false;
        };
        self.fully_scanned_player_scripts.contains(script)
            && !self
                .unresolved_player_outputs
                .values()
                .any(|record| record.script.eq_ignore_ascii_case(script))
    }

    async fn issue_move_assets(&mut self, spendable: &[VtxoRecord]) -> Result<()> {
        let required = self.required_funding();
        let selected = self.registration_inputs(spendable).ok_or_else(|| {
            anyhow!(
                "need BTC-only inputs totaling exactly {required} sats or leaving at least {} sats of change",
                self.params.vtxo_min_sats
            )
        })?;

        self.phase = Phase::Issuing;
        self.sending = true;
        let built = txbuild::build_move_asset_issuance_tx(
            &self.keys,
            &self.params,
            &self.info,
            self.game_address,
            &selected,
        );
        let result = match built {
            Ok((ark_tx, checkpoints, asset_ids)) => {
                txbuild::prepare_tx(&self.keys, ark_tx, checkpoints)
                    .map(|submission| (submission, asset_ids))
            }
            Err(err) => Err(err),
        };
        self.sending = false;

        let (submission, asset_ids) = result?;
        let txid = submission.txid;
        self.move_assets = Some(asset_ids);
        self.registered_players.insert(txid, self.player_script());
        self.move_balances = [txbuild::MOVE_SUPPLY; 4];
        self.next_sequence = 0;
        self.log_line(format!(
            "issuance {} prepared; persisting before submission",
            short_txid(&txid)
        ));
        self.pending_action = Some(PendingAction::UnknownIssuance {
            submission,
            asset_ids,
        });
        Ok(())
    }

    fn registration_inputs(&self, spendable: &[VtxoRecord]) -> Option<Vec<VtxoRecord>> {
        let required = self.required_funding();
        let minimum_change = self.params.vtxo_min_sats.max(1);
        let mut candidates: Vec<_> = spendable
            .iter()
            .filter(|record| record.assets.is_empty())
            .filter(|record| safe_wallet_input(record))
            .filter(|record| {
                !self
                    .excluded_outpoints
                    .contains(&record.outpoint.to_string())
            })
            .cloned()
            .collect();

        if let Some(record) = candidates
            .iter()
            .filter(|record| valid_registration_total(record.amount_sats, required, minimum_change))
            .min_by_key(|record| record.amount_sats)
        {
            return Some(vec![record.clone()]);
        }

        candidates.sort_by_key(|record| std::cmp::Reverse(record.amount_sats));
        let mut selected = Vec::new();
        let mut total = 0u64;
        for record in candidates {
            total = total.saturating_add(record.amount_sats);
            selected.push(record);
            if valid_registration_total(total, required, minimum_change) {
                return Some(selected);
            }
        }
        None
    }

    async fn sweep_wallet(&mut self, spendable: &[VtxoRecord], destination: &str) -> Result<()> {
        let destination = destination.trim();
        let expected_prefix = if self.params.network == bitcoin::Network::Bitcoin {
            "ark1"
        } else {
            "tark1"
        };
        if !destination
            .to_ascii_lowercase()
            .starts_with(expected_prefix)
        {
            return Err(anyhow!(
                "destination must be a {expected_prefix} address for {}",
                self.params.network_name
            ));
        }
        let recipient = ark_core::ArkAddress::decode(destination)
            .map_err(|error| anyhow!("invalid destination Ark address: {error}"))?;
        if recipient.server() != self.params.signer_pk {
            return Err(anyhow!(
                "destination belongs to a different Arkade operator"
            ));
        }
        if recipient == self.player_address() {
            return Err(anyhow!(
                "destination must differ from this wallet; sweeping to self would merge the game carrier"
            ));
        }

        let selected: Vec<_> = spendable
            .iter()
            .filter(|record| safe_wallet_input(record))
            .filter(|record| {
                !self
                    .excluded_outpoints
                    .contains(&record.outpoint.to_string())
            })
            .cloned()
            .collect();
        if selected.is_empty() {
            return Err(anyhow!("no safely spendable VTXOs to sweep"));
        }
        if selected.len() > 20 {
            return Err(anyhow!(
                "wallet has {} spendable VTXOs; sweep at most 20 after consolidating",
                selected.len()
            ));
        }
        let total: u64 = selected.iter().map(|record| record.amount_sats).sum();
        self.log_line(format!(
            "sweeping {total} sats and all carried assets to {destination}"
        ));
        self.sending = true;
        let built =
            txbuild::build_sweep_tx(&self.keys, &self.params, &self.info, &selected, recipient);
        let result = match built {
            Ok((ark_tx, checkpoints)) => txbuild::prepare_tx(&self.keys, ark_tx, checkpoints),
            Err(error) => Err(error),
        };
        self.sending = false;

        let submission = result?;
        self.log_line(format!(
            "wallet sweep {} prepared; persisting before submission",
            short_txid(&submission.txid)
        ));
        self.pending_action = Some(PendingAction::UnknownSweep { submission });
        Ok(())
    }

    async fn flush_one_move(&mut self, spendable: &[VtxoRecord]) {
        let Some(direction) = self.pending_dirs.pop_front() else {
            return;
        };
        if self.move_balances[direction as usize] == 0 {
            self.log_line(format!(
                "no {} assets left",
                crate::game::DIRECTIONS[direction as usize].to_ascii_uppercase()
            ));
            return;
        }
        let asset_id = self.move_assets.expect("checked")[direction as usize];
        let asset_text = asset_id.to_string();
        let Some(carrier) = spendable
            .iter()
            .filter(|record| safe_wallet_input(record))
            .filter(|record| record.amount_sats == self.params.dust_sats)
            .filter(|record| {
                !self
                    .excluded_outpoints
                    .contains(&record.outpoint.to_string())
            })
            .filter(|record| {
                record
                    .assets
                    .iter()
                    .any(|(id, amount)| id == &asset_text && *amount > 0)
            })
            .max_by_key(|record| record.amount_sats)
            .cloned()
        else {
            self.pending_dirs.push_front(direction);
            self.log_line("waiting for the move asset carrier");
            return;
        };

        self.sending = true;
        let built = txbuild::build_move_burn_tx(
            &self.keys,
            &self.params,
            &self.info,
            &carrier,
            asset_id,
            self.next_sequence,
        );
        let result = match built {
            Ok((ark_tx, checkpoints)) => txbuild::prepare_tx(&self.keys, ark_tx, checkpoints),
            Err(err) => Err(err),
        };
        self.sending = false;

        match result {
            Ok(submission) => {
                let txid = submission.txid;
                self.log_line(format!(
                    "move {} prepared; persisting before submission",
                    short_txid(&txid)
                ));
                self.pending_action = Some(PendingAction::UnknownMove {
                    submission,
                    direction,
                    sequence: self.next_sequence,
                });
            }
            Err(err) => {
                self.pending_dirs.push_front(direction);
                self.handle_send_error("move", &err);
            }
        }
    }

    fn complete_move(&mut self, direction: u8, sequence: u32, txid: Txid) {
        self.next_sequence = self.next_sequence.max(sequence.saturating_add(1));
        self.move_balances[direction as usize] =
            self.move_balances[direction as usize].saturating_sub(1);
        self.log_line(format!(
            "burned {} #{} in {}",
            crate::game::DIRECTIONS[direction as usize].to_ascii_uppercase(),
            sequence.saturating_add(1),
            short_txid(&txid)
        ));
    }

    async fn resume_pending(&mut self, spendable: &[VtxoRecord]) {
        let Some(action) = self.pending_action.clone() else {
            return;
        };
        let observed = match &action {
            PendingAction::Issuance { asset_ids, .. } => spendable.iter().any(|record| {
                record
                    .assets
                    .iter()
                    .any(|(id, _)| asset_ids.iter().any(|asset| id == &asset.to_string()))
            }),
            PendingAction::Move { pending, .. } => {
                self.events.iter().any(|event| event.txid == pending.txid)
                    || self.transaction_is_indexed(pending.txid).await
            }
            PendingAction::UnknownIssuance { asset_ids, .. } => spendable.iter().any(|record| {
                record
                    .assets
                    .iter()
                    .any(|(id, _)| asset_ids.iter().any(|asset| id == &asset.to_string()))
            }),
            PendingAction::UnknownMove { submission, .. } => {
                self.events
                    .iter()
                    .any(|event| event.txid == submission.txid)
                    || self.transaction_is_indexed(submission.txid).await
            }
            PendingAction::Sweep { pending } => self.transaction_is_indexed(pending.txid).await,
            PendingAction::UnknownSweep { submission } => {
                self.transaction_is_indexed(submission.txid).await
            }
        };
        if observed {
            self.finish_pending(action, false);
            return;
        }

        if matches!(
            action,
            PendingAction::UnknownIssuance { .. }
                | PendingAction::UnknownMove { .. }
                | PendingAction::UnknownSweep { .. }
        ) {
            self.resume_unknown(action).await;
            return;
        }

        let pending = match &action {
            PendingAction::Issuance { pending, .. }
            | PendingAction::Move { pending, .. }
            | PendingAction::Sweep { pending } => Some(pending),
            PendingAction::UnknownIssuance { .. }
            | PendingAction::UnknownMove { .. }
            | PendingAction::UnknownSweep { .. } => None,
        };
        let Some(pending) = pending else {
            return;
        };
        self.sending = true;
        let result = txbuild::finalize_pending(&self.keys, &self.rest, pending).await;
        self.sending = false;
        match result {
            Ok(()) => self.finish_pending(action, true),
            Err(err) => self.report_error("pending finalization", &err),
        }
    }

    async fn resume_unknown(&mut self, action: PendingAction) {
        let submission = match &action {
            PendingAction::UnknownIssuance { submission, .. }
            | PendingAction::UnknownMove { submission, .. }
            | PendingAction::UnknownSweep { submission } => submission,
            _ => return,
        };
        self.sending = true;
        let result =
            txbuild::retry_unknown_submission(&self.keys, &self.params, &self.rest, submission)
                .await;
        self.sending = false;

        match result {
            Ok(txbuild::RunTxStatus::Finalized(_)) => self.finish_pending(action, true),
            Ok(txbuild::RunTxStatus::Pending(pending)) => {
                self.pending_action = Some(match action {
                    PendingAction::UnknownIssuance { asset_ids, .. } => {
                        PendingAction::Issuance { pending, asset_ids }
                    }
                    PendingAction::UnknownMove {
                        direction,
                        sequence,
                        ..
                    } => PendingAction::Move {
                        pending,
                        direction,
                        sequence,
                    },
                    PendingAction::UnknownSweep { .. } => PendingAction::Sweep { pending },
                    _ => return,
                });
            }
            Ok(txbuild::RunTxStatus::SubmissionUnknown(submission)) => {
                self.pending_action = Some(match action {
                    PendingAction::UnknownIssuance { asset_ids, .. } => {
                        PendingAction::UnknownIssuance {
                            submission,
                            asset_ids,
                        }
                    }
                    PendingAction::UnknownMove {
                        direction,
                        sequence,
                        ..
                    } => PendingAction::UnknownMove {
                        submission,
                        direction,
                        sequence,
                    },
                    PendingAction::UnknownSweep { .. } => {
                        PendingAction::UnknownSweep { submission }
                    }
                    _ => return,
                });
            }
            Err(err) => self.report_error("submission recovery", &err),
        }
    }

    fn finish_pending(&mut self, action: PendingAction, optimistic_balance: bool) {
        match action {
            PendingAction::Issuance { pending, asset_ids } => {
                self.move_assets = Some(asset_ids);
                self.registered_players
                    .insert(pending.txid, self.player_script());
                self.log_line(format!("issuance {} finalized", short_txid(&pending.txid)));
            }
            PendingAction::Move {
                pending,
                direction,
                sequence,
            } => {
                if optimistic_balance {
                    self.complete_move(direction, sequence, pending.txid);
                } else {
                    self.next_sequence = self.next_sequence.max(sequence.saturating_add(1));
                    self.log_line(format!("move {} finalized", short_txid(&pending.txid)));
                }
            }
            PendingAction::UnknownIssuance {
                submission,
                asset_ids,
            } => {
                let txid = submission.txid;
                self.move_assets = Some(asset_ids);
                self.registered_players.insert(txid, self.player_script());
                self.log_line(format!(
                    "issuance {} appeared in indexer",
                    short_txid(&txid)
                ));
            }
            PendingAction::UnknownMove {
                submission,
                direction,
                sequence,
            } => {
                let txid = submission.txid;
                if optimistic_balance {
                    self.complete_move(direction, sequence, txid);
                } else {
                    self.next_sequence = self.next_sequence.max(sequence.saturating_add(1));
                    self.log_line(format!("move {} appeared in indexer", short_txid(&txid)));
                }
            }
            PendingAction::Sweep { pending } => {
                self.log_line(format!(
                    "wallet sweep {} finalized",
                    short_txid(&pending.txid)
                ));
            }
            PendingAction::UnknownSweep { submission } => {
                self.log_line(format!(
                    "wallet sweep {} appeared in indexer",
                    short_txid(&submission.txid)
                ));
            }
        }
        self.pending_action = None;
    }

    async fn transaction_is_indexed(&self, txid: Txid) -> bool {
        self.rest
            .get_virtual_txs(&[txid.to_string()])
            .await
            .is_ok_and(|txs| {
                txs.into_iter()
                    .any(|psbt| psbt.unsigned_tx.compute_txid() == txid)
            })
    }

    fn handle_send_error(&mut self, action: &str, err: &anyhow::Error) {
        let text = format!("{err:#}");
        if let Some(outpoint) = extract_spent_outpoint(&text) {
            self.excluded_outpoints.insert(outpoint);
        }
        self.report_error(action, err);
    }

    fn report_error(&mut self, action: &str, err: &anyhow::Error) {
        let text = format!("{action}: {err:#}");
        if self.last_error.as_deref() != Some(text.as_str()) {
            self.log_line(text.clone());
            self.last_error = Some(text);
        }
    }

    fn clear_error(&mut self) {
        self.last_error = None;
    }

    async fn refresh_game(&mut self) -> Result<()> {
        let periodic_full = self.game_sync_count % 250 == 0;
        let new_players = self
            .refresh_registrations(!self.initial_registry_sync || periodic_full)
            .await?;
        let accepted_moves = self
            .refresh_player_moves(!self.initial_player_sync || periodic_full || new_players > 0)
            .await?;
        self.initial_registry_sync = true;
        self.initial_player_sync = true;
        self.game_sync_count = self.game_sync_count.wrapping_add(1);
        self.last_sync_ms = now_ms();
        if new_players > 0 {
            self.log_line(format!("discovered {new_players} new players"));
        }
        if accepted_moves > 0 {
            self.log_line(format!("synced {accepted_moves} protocol burns"));
        }
        self.reconcile_next_sequence();
        Ok(())
    }

    async fn refresh_registrations(&mut self, force_full: bool) -> Result<u32> {
        let candidates = self.registry_records_to_check(force_full).await?;
        for record in &candidates {
            self.unresolved_registry_outputs
                .entry(record.outpoint)
                .or_insert_with(|| record.clone());
        }
        if candidates.is_empty() {
            return Ok(0);
        }

        let mut records_by_txid: HashMap<Txid, Vec<VtxoRecord>> = HashMap::new();
        for record in &candidates {
            records_by_txid
                .entry(record.outpoint.txid)
                .or_default()
                .push(record.clone());
        }
        let txids: Vec<_> = records_by_txid.keys().copied().collect();
        let txs = self.fetch_txs(&txids).await;
        let mut new_players = 0u32;

        for (txid, records) in records_by_txid {
            let Some(psbt) = txs.get(&txid) else {
                continue;
            };
            let matching_records: Vec<_> = records
                .iter()
                .filter(|record| record.outpoint.vout == 0)
                .collect();
            let player_script = registration_player_script(
                &psbt.unsigned_tx,
                &self.game_script,
                self.params.dust_sats,
            );
            let Some(player_script) = player_script else {
                self.invalid_registrations.insert(txid);
                self.mark_registry_outputs_seen(&records);
                continue;
            };
            if matching_records.len() != 1 || !matching_records[0].assets.is_empty() {
                self.invalid_registrations.insert(txid);
                self.mark_registry_outputs_seen(&records);
                continue;
            }
            self.invalid_registrations.remove(&txid);
            if self
                .registered_players
                .insert(txid, player_script)
                .is_none()
            {
                new_players = new_players.saturating_add(1);
            }
            self.mark_registry_outputs_seen(&records);
        }

        Ok(new_players)
    }

    async fn registry_records_to_check(&self, force_full: bool) -> Result<Vec<VtxoRecord>> {
        let mut index = 1;
        let mut records: Vec<_> = self.unresolved_registry_outputs.values().cloned().collect();
        let mut collected: HashSet<_> = self.unresolved_registry_outputs.keys().copied().collect();
        loop {
            let page = self
                .rest
                .get_vtxos_page(&self.game_script, "", INDEX_PAGE_SIZE, index)
                .await?;
            let page_was_known = !page.vtxos.is_empty()
                && page.vtxos.iter().all(|record| {
                    self.seen_registry_outputs.contains(&record.outpoint)
                        || self
                            .unresolved_registry_outputs
                            .contains_key(&record.outpoint)
                });
            for record in page.vtxos {
                if !self.seen_registry_outputs.contains(&record.outpoint)
                    && collected.insert(record.outpoint)
                {
                    records.push(record);
                }
            }
            if (!force_full && self.initial_registry_sync && page_was_known)
                || page.next <= page.current
                || page.next <= 0
                || page.current >= page.total
            {
                break;
            }
            index = page.next;
        }
        Ok(records)
    }

    fn mark_registry_outputs_seen(&mut self, records: &[VtxoRecord]) {
        for record in records {
            self.seen_registry_outputs.insert(record.outpoint);
            self.unresolved_registry_outputs.remove(&record.outpoint);
        }
    }

    async fn refresh_player_moves(&mut self, force_full: bool) -> Result<u32> {
        let (candidates, fully_scanned) = self.player_records_to_check(force_full).await?;
        for record in &candidates {
            self.unresolved_player_outputs
                .entry(record.outpoint)
                .or_insert_with(|| record.clone());
        }
        if candidates.is_empty() {
            self.fully_scanned_player_scripts.extend(fully_scanned);
            return Ok(0);
        }

        let mut records_by_txid: HashMap<Txid, Vec<VtxoRecord>> = HashMap::new();
        for record in &candidates {
            records_by_txid
                .entry(record.outpoint.txid)
                .or_default()
                .push(record.clone());
        }
        let txids: Vec<_> = records_by_txid.keys().copied().collect();
        let txs = self.fetch_txs(&txids).await;
        let mut accepted = 0u32;

        for (txid, records) in records_by_txid {
            let Some(psbt) = txs.get(&txid) else {
                continue;
            };
            let Some(burn) = move_burn_from_tx(&psbt.unsigned_tx) else {
                self.mark_player_outputs_seen(&records);
                continue;
            };
            let player_script = match self.issuance_player_script(burn.asset_id.txid).await {
                Ok(Some(script)) => script,
                Ok(None) => {
                    self.mark_player_outputs_seen(&records);
                    continue;
                }
                Err(_) => continue,
            };
            let player_outputs: Vec<_> = psbt
                .unsigned_tx
                .output
                .iter()
                .enumerate()
                .filter(|(_, output)| {
                    output.script_pubkey.to_hex_string() == player_script
                        && output.value.to_sat() > 0
                })
                .collect();
            let canonical_carrier = player_outputs.len() == 1
                && player_outputs[0].0 == 0
                && player_outputs[0].1.value.to_sat() == self.params.dust_sats;
            let assets_stay_with_player = burn
                .preserved_output_indexes
                .iter()
                .all(|index| *index == 0);
            let indexed_carrier = records.iter().any(|record| {
                record.outpoint.vout == 0
                    && record.amount_sats == self.params.dust_sats
                    && record.script.eq_ignore_ascii_case(&player_script)
            });
            if !canonical_carrier || !assets_stay_with_player || !indexed_carrier {
                self.mark_player_outputs_seen(&records);
                continue;
            }

            if self.seen_move_txs.insert(txid) {
                self.events.push(MoveEvent {
                    txid,
                    player: burn.asset_id.txid,
                    sequence: burn.receipt.sequence,
                    direction: burn.asset_id.group_index as u8,
                });
                accepted = accepted.saturating_add(1);
            }
            self.mark_player_outputs_seen(&records);
        }

        self.fully_scanned_player_scripts.extend(fully_scanned);
        Ok(accepted)
    }

    async fn player_records_to_check(
        &self,
        force_full: bool,
    ) -> Result<(Vec<VtxoRecord>, Vec<String>)> {
        let scripts: Vec<_> = self
            .registered_players
            .values()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut records: Vec<_> = self.unresolved_player_outputs.values().cloned().collect();
        let mut collected: HashSet<_> = self.unresolved_player_outputs.keys().copied().collect();
        let mut fully_scanned = Vec::new();

        for scripts in scripts.chunks(PLAYER_SCRIPT_BATCH_SIZE) {
            let scan_all = force_full
                || scripts
                    .iter()
                    .any(|script| !self.fully_scanned_player_scripts.contains(script));
            let mut index = 1;
            loop {
                let page = self
                    .rest
                    .get_vtxos_page_many(scripts, "", INDEX_PAGE_SIZE, index)
                    .await?;
                let page_was_known = !page.vtxos.is_empty()
                    && page.vtxos.iter().all(|record| {
                        self.seen_player_outputs.contains(&record.outpoint)
                            || self
                                .unresolved_player_outputs
                                .contains_key(&record.outpoint)
                    });
                for record in page.vtxos {
                    if !self.seen_player_outputs.contains(&record.outpoint)
                        && collected.insert(record.outpoint)
                    {
                        records.push(record);
                    }
                }
                if (!scan_all && self.initial_player_sync && page_was_known)
                    || page.next <= page.current
                    || page.next <= 0
                    || page.current >= page.total
                {
                    break;
                }
                index = page.next;
            }
            if scan_all {
                fully_scanned.extend_from_slice(scripts);
            }
        }
        Ok((records, fully_scanned))
    }

    fn mark_player_outputs_seen(&mut self, records: &[VtxoRecord]) {
        for record in records {
            self.seen_player_outputs.insert(record.outpoint);
            self.unresolved_player_outputs.remove(&record.outpoint);
        }
    }

    async fn fetch_txs(&self, txids: &[Txid]) -> HashMap<Txid, bitcoin::Psbt> {
        let mut out = HashMap::new();
        for chunk in txids.chunks(40) {
            let ids: Vec<String> = chunk.iter().map(ToString::to_string).collect();
            match self.rest.get_virtual_txs(&ids).await {
                Ok(txs) => {
                    for psbt in txs {
                        out.insert(psbt.unsigned_tx.compute_txid(), psbt);
                    }
                }
                Err(_) => {
                    for id in ids {
                        if let Ok(txs) = self.rest.get_virtual_txs(&[id]).await {
                            for psbt in txs {
                                out.insert(psbt.unsigned_tx.compute_txid(), psbt);
                            }
                        }
                    }
                }
            }
        }
        out
    }

    async fn issuance_player_script(&mut self, txid: Txid) -> Result<Option<String>> {
        if let Some(script) = self.registered_players.get(&txid) {
            return Ok(Some(script.clone()));
        }
        if self.invalid_registrations.contains(&txid) {
            return Ok(None);
        }
        let txs = self.rest.get_virtual_txs(&[txid.to_string()]).await?;
        let psbt = txs
            .into_iter()
            .find(|psbt| psbt.unsigned_tx.compute_txid() == txid)
            .ok_or_else(|| anyhow!("player registration {txid} is not indexed yet"))?;
        let player_script =
            registration_player_script(&psbt.unsigned_tx, &self.game_script, self.params.dust_sats);
        if let Some(script) = &player_script {
            self.registered_players.insert(txid, script.clone());
        } else {
            self.invalid_registrations.insert(txid);
        }
        Ok(player_script)
    }

    fn player_id(&self) -> Option<Txid> {
        self.move_assets.map(|assets| assets[0].txid)
    }

    fn player_snapshots(&self) -> Vec<PlayerSnapshot> {
        let mut streams = canonical_moves(&self.events);
        for player in self.registered_players.keys() {
            streams.entry(*player).or_default();
        }
        if let Some(me) = self.player_id() {
            streams.entry(me).or_default();
        }
        let me = self.player_id();
        streams
            .into_iter()
            .map(|(id, moves)| {
                let mut state = PlayerState::default();
                for direction in moves {
                    state.apply(direction);
                }
                PlayerSnapshot {
                    id: id.to_string(),
                    x: state.x,
                    y: state.y,
                    moves: state.moves,
                    laps: state.laps,
                    is_me: Some(id) == me,
                }
            })
            .collect()
    }

    fn pending_txid(&self) -> Option<String> {
        self.pending_action.as_ref().map(|action| match action {
            PendingAction::Issuance { pending, .. }
            | PendingAction::Move { pending, .. }
            | PendingAction::Sweep { pending } => pending.txid.to_string(),
            PendingAction::UnknownIssuance { submission, .. }
            | PendingAction::UnknownMove { submission, .. }
            | PendingAction::UnknownSweep { submission } => submission.txid.to_string(),
        })
    }

    fn wallet_action(&self) -> &'static str {
        match self.pending_action {
            Some(PendingAction::Sweep { .. }) => "finalizing-sweep",
            Some(PendingAction::UnknownSweep { .. }) => "recovering-sweep",
            Some(PendingAction::Issuance { .. }) | Some(PendingAction::Move { .. }) => {
                "finalizing-game-tx"
            }
            Some(PendingAction::UnknownIssuance { .. })
            | Some(PendingAction::UnknownMove { .. }) => "recovering-game-tx",
            None if self.sending => "submitting",
            None => "idle",
        }
    }

    fn wallet_snapshots(&self) -> Vec<WalletVtxoSnapshot> {
        let now = (now_ms() / 1_000) as i64;
        let mut records: Vec<_> =
            self.wallet_records
                .iter()
                .map(|record| {
                    let status = if record.is_unrolled {
                        "unrolled"
                    } else if record.is_swept {
                        "swept"
                    } else if record.is_spent {
                        "spent"
                    } else if record.amount_sats < self.params.dust_sats
                        || record.expires_at.is_some_and(|expires| expires <= now)
                    {
                        "recoverable"
                    } else if record.expires_at.is_some_and(|expires| {
                        expires <= now.saturating_add(MIN_INPUT_LIFETIME_SECS)
                    }) {
                        "expiring"
                    } else if record.is_preconfirmed {
                        "preconfirmed"
                    } else {
                        "confirmed"
                    };
                    WalletVtxoSnapshot {
                        outpoint: record.outpoint.to_string(),
                        amount: record.amount_sats,
                        status: status.to_string(),
                        assets: record
                            .assets
                            .iter()
                            .map(|(asset_id, amount)| WalletAssetSnapshot {
                                asset_id: asset_id.clone(),
                                amount: *amount,
                            })
                            .collect(),
                        expires_at: record.expires_at,
                        spent_by: record.spent_by.clone(),
                    }
                })
                .collect();
        records.sort_by(|left, right| left.outpoint.cmp(&right.outpoint));
        records
    }

    pub fn snapshot(&self, version: &str) -> Snapshot {
        Snapshot {
            version: version.to_string(),
            server: self.rest.base().to_string(),
            operator_version: self.params.server_version.clone(),
            signer: self.params.signer_pk.to_string(),
            network: self.params.network_name.clone(),
            phase: match self.phase {
                Phase::FundWallet => "fund-wallet",
                Phase::Issuing => "issuing",
                Phase::Syncing => "syncing",
                Phase::Playing => "playing",
                Phase::OutOfMoves => "out-of-moves",
            }
            .to_string(),
            address: self.player_address().encode(),
            game_address: self.game_address(),
            player_id: self.player_id().map(|id| id.to_string()),
            balance: self.balance_cache,
            known_balance: self
                .wallet_records
                .iter()
                .filter(|record| !record.is_spent && !record.is_swept && !record.is_unrolled)
                .map(|record| record.amount_sats)
                .sum(),
            required_funding: self.required_funding(),
            funding_ready: self.funding_ready,
            registration_cost: self.params.dust_sats,
            reusable_carrier: self.params.dust_sats,
            move_balances: self.move_balances,
            sending: self.sending,
            pending: self.pending_action.is_some(),
            pending_txid: self.pending_txid(),
            wallet_action: self.wallet_action().to_string(),
            wallet_vtxos: self.wallet_snapshots(),
            queued: self.pending_dirs.len() as u32,
            players: self.player_snapshots(),
            maze_width: crate::game::MAZE_W,
            maze_height: crate::game::MAZE_H,
            walls: crate::game::walls(),
            start: [crate::game::START.0, crate::game::START.1],
            goal: [crate::game::GOAL.0, crate::game::GOAL.1],
            events: self.events.len() as u32,
            wallet_sync_ms: self.wallet_sync_ms,
            last_sync_ms: self.last_sync_ms,
            last_error: self.last_error.clone(),
            log: self.log.clone(),
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSnapshot {
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub moves: u32,
    pub laps: u32,
    pub is_me: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WalletAssetSnapshot {
    pub asset_id: String,
    pub amount: u64,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WalletVtxoSnapshot {
    pub outpoint: String,
    pub amount: u64,
    pub status: String,
    pub assets: Vec<WalletAssetSnapshot>,
    pub expires_at: Option<i64>,
    pub spent_by: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub version: String,
    pub server: String,
    pub operator_version: String,
    pub signer: String,
    pub network: String,
    pub phase: String,
    pub address: String,
    pub game_address: String,
    pub player_id: Option<String>,
    pub balance: u64,
    pub known_balance: u64,
    pub required_funding: u64,
    pub funding_ready: bool,
    pub registration_cost: u64,
    pub reusable_carrier: u64,
    pub move_balances: [u64; 4],
    pub sending: bool,
    pub pending: bool,
    pub pending_txid: Option<String>,
    pub wallet_action: String,
    pub wallet_vtxos: Vec<WalletVtxoSnapshot>,
    pub queued: u32,
    pub players: Vec<PlayerSnapshot>,
    pub maze_width: i32,
    pub maze_height: i32,
    pub walls: Vec<[i32; 2]>,
    pub start: [i32; 2],
    pub goal: [i32; 2],
    pub events: u32,
    pub wallet_sync_ms: u64,
    pub last_sync_ms: u64,
    pub last_error: Option<String>,
    pub log: Vec<String>,
}

fn pending_action_to_journal(action: &PendingAction) -> PendingJournalAction {
    match action {
        PendingAction::Issuance { pending, asset_ids } => PendingJournalAction::Issuance {
            transaction: journal_transaction(
                JournalStage::Finalizing,
                pending.txid,
                &pending.signed_ark,
                &pending.checkpoints,
                &pending.last_error,
            ),
            asset_ids: (*asset_ids).map(|asset| asset.to_string()),
        },
        PendingAction::Move {
            pending,
            direction,
            sequence,
        } => PendingJournalAction::Move {
            transaction: journal_transaction(
                JournalStage::Finalizing,
                pending.txid,
                &pending.signed_ark,
                &pending.checkpoints,
                &pending.last_error,
            ),
            direction: *direction,
            sequence: *sequence,
        },
        PendingAction::UnknownIssuance {
            submission,
            asset_ids,
        } => PendingJournalAction::Issuance {
            transaction: journal_transaction(
                JournalStage::Prepared,
                submission.txid,
                &submission.signed_ark,
                &submission.checkpoints,
                &submission.last_error,
            ),
            asset_ids: (*asset_ids).map(|asset| asset.to_string()),
        },
        PendingAction::UnknownMove {
            submission,
            direction,
            sequence,
        } => PendingJournalAction::Move {
            transaction: journal_transaction(
                JournalStage::Prepared,
                submission.txid,
                &submission.signed_ark,
                &submission.checkpoints,
                &submission.last_error,
            ),
            direction: *direction,
            sequence: *sequence,
        },
        PendingAction::Sweep { pending } => PendingJournalAction::Sweep {
            transaction: journal_transaction(
                JournalStage::Finalizing,
                pending.txid,
                &pending.signed_ark,
                &pending.checkpoints,
                &pending.last_error,
            ),
        },
        PendingAction::UnknownSweep { submission } => PendingJournalAction::Sweep {
            transaction: journal_transaction(
                JournalStage::Prepared,
                submission.txid,
                &submission.signed_ark,
                &submission.checkpoints,
                &submission.last_error,
            ),
        },
    }
}

fn journal_transaction(
    stage: JournalStage,
    txid: Txid,
    signed_ark: &Psbt,
    checkpoints: &[Psbt],
    last_error: &str,
) -> JournalTransaction {
    let base64 = base64::engine::general_purpose::STANDARD;
    JournalTransaction {
        stage,
        txid: txid.to_string(),
        signed_ark: base64.encode(signed_ark.serialize()),
        checkpoints: checkpoints
            .iter()
            .map(|checkpoint| base64.encode(checkpoint.serialize()))
            .collect(),
        last_error: last_error.to_string(),
    }
}

fn pending_action_from_journal(action: PendingJournalAction) -> Result<PendingAction> {
    match action {
        PendingJournalAction::Issuance {
            transaction,
            asset_ids,
        } => {
            let transaction = restore_transaction(transaction)?;
            let txid = restored_transaction_txid(&transaction);
            let asset_ids = restore_asset_ids(asset_ids, txid)?;
            Ok(match transaction {
                RestoredTransaction::Prepared(submission) => PendingAction::UnknownIssuance {
                    submission,
                    asset_ids,
                },
                RestoredTransaction::Finalizing(pending) => {
                    PendingAction::Issuance { pending, asset_ids }
                }
            })
        }
        PendingJournalAction::Move {
            transaction,
            direction,
            sequence,
        } => {
            if direction > crate::game::DIR_LEFT {
                return Err(anyhow!("pending move has an invalid direction"));
            }
            Ok(match restore_transaction(transaction)? {
                RestoredTransaction::Prepared(submission) => PendingAction::UnknownMove {
                    submission,
                    direction,
                    sequence,
                },
                RestoredTransaction::Finalizing(pending) => PendingAction::Move {
                    pending,
                    direction,
                    sequence,
                },
            })
        }
        PendingJournalAction::Sweep { transaction } => {
            Ok(match restore_transaction(transaction)? {
                RestoredTransaction::Prepared(submission) => {
                    PendingAction::UnknownSweep { submission }
                }
                RestoredTransaction::Finalizing(pending) => PendingAction::Sweep { pending },
            })
        }
    }
}

fn restore_transaction(transaction: JournalTransaction) -> Result<RestoredTransaction> {
    let txid: Txid = transaction.txid.parse().context("parse pending txid")?;
    let base64 = base64::engine::general_purpose::STANDARD;
    let signed_ark = Psbt::deserialize(
        &base64
            .decode(&transaction.signed_ark)
            .context("decode pending Ark PSBT")?,
    )
    .context("parse pending Ark PSBT")?;
    if signed_ark.unsigned_tx.compute_txid() != txid {
        return Err(anyhow!("pending Ark PSBT does not match its txid"));
    }
    if transaction.checkpoints.is_empty() || transaction.checkpoints.len() > 20 {
        return Err(anyhow!(
            "pending transaction must contain between 1 and 20 checkpoints"
        ));
    }
    let checkpoints = transaction
        .checkpoints
        .iter()
        .map(|checkpoint| {
            let bytes = base64
                .decode(checkpoint)
                .context("decode pending checkpoint PSBT")?;
            Psbt::deserialize(&bytes).context("parse pending checkpoint PSBT")
        })
        .collect::<Result<Vec<_>>>()?;
    if signed_ark.unsigned_tx.input.len() != checkpoints.len()
        || checkpoints
            .iter()
            .any(|checkpoint| checkpoint.unsigned_tx.input.len() != 1)
    {
        return Err(anyhow!(
            "pending transaction checkpoint count is inconsistent"
        ));
    }

    Ok(match transaction.stage {
        JournalStage::Prepared => RestoredTransaction::Prepared(txbuild::UnknownSubmission {
            txid,
            signed_ark,
            checkpoints,
            last_error: transaction.last_error,
        }),
        JournalStage::Finalizing => RestoredTransaction::Finalizing(txbuild::PendingFinalize {
            txid,
            signed_ark,
            checkpoints,
            last_error: transaction.last_error,
        }),
    })
}

fn restore_asset_ids(raw: [String; 4], txid: Txid) -> Result<[AssetId; 4]> {
    let parsed = raw
        .iter()
        .map(|asset| {
            txbuild::parse_asset_id_pub(asset)
                .ok_or_else(|| anyhow!("pending issuance contains an invalid asset ID"))
        })
        .collect::<Result<Vec<_>>>()?;
    let assets: [AssetId; 4] = parsed
        .try_into()
        .map_err(|_| anyhow!("pending issuance requires four asset IDs"))?;
    for (index, asset) in assets.iter().enumerate() {
        if asset.txid != txid || asset.group_index != index as u16 {
            return Err(anyhow!(
                "pending issuance asset IDs do not match the issuance transaction"
            ));
        }
    }
    Ok(assets)
}

fn restored_transaction_txid(transaction: &RestoredTransaction) -> Txid {
    match transaction {
        RestoredTransaction::Prepared(submission) => submission.txid,
        RestoredTransaction::Finalizing(pending) => pending.txid,
    }
}

fn pending_action_txid(action: &PendingAction) -> Txid {
    match action {
        PendingAction::Issuance { pending, .. }
        | PendingAction::Move { pending, .. }
        | PendingAction::Sweep { pending } => pending.txid,
        PendingAction::UnknownIssuance { submission, .. }
        | PendingAction::UnknownMove { submission, .. }
        | PendingAction::UnknownSweep { submission } => submission.txid,
    }
}

fn short_txid(txid: &Txid) -> String {
    txid.to_string()[..8].to_string()
}

fn safe_wallet_input(record: &VtxoRecord) -> bool {
    if record.is_swept || record.is_unrolled {
        return false;
    }
    let now = (now_ms() / 1_000) as i64;
    record
        .expires_at
        .is_some_and(|expires_at| expires_at > now.saturating_add(MIN_INPUT_LIFETIME_SECS))
}

fn valid_registration_total(total: u64, required: u64, minimum_change: u64) -> bool {
    total == required || total >= required.saturating_add(minimum_change)
}

/// Recompute the registry address for an operator using Arkade's standard NUMS
/// owner. This is exposed for the verification example and documentation.
pub fn nums_registry_address(params: &ServerParams) -> Result<ark_core::ArkAddress> {
    let nums = ark_core::UNSPENDABLE_KEY
        .parse::<bitcoin::PublicKey>()
        .context("parse Arkade NUMS key")?;
    let owner = nums.inner.x_only_public_key().0;
    let vtxo = ark_core::Vtxo::new_default(
        &bitcoin::key::Secp256k1::new(),
        params.signer_pk,
        owner,
        params.unilateral_exit_delay,
        params.network,
    )?;
    Ok(vtxo.to_ark_address())
}

/// Pull a `txid:vout already spent` outpoint out of an operator error body.
pub fn extract_spent_outpoint(text: &str) -> Option<String> {
    let idx = text.find("already spent")?;
    let token = text[..idx]
        .split(|c: char| !c.is_ascii_hexdigit() && c != ':' && c != '.')
        .rfind(|part| !part.is_empty())?;
    let (txid, vout) = token.split_once(':')?;
    if txid.len() == 64
        && txid.chars().all(|c| c.is_ascii_hexdigit())
        && vout.parse::<u32>().is_ok()
    {
        Some(token.to_string())
    } else {
        None
    }
}

pub fn now_ms() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now() as u64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::hashes::Hash;

    fn test_psbt(byte: u8) -> Psbt {
        Psbt::from_unsigned_tx(bitcoin::Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn {
                previous_output: OutPoint {
                    txid: Txid::from_byte_array([byte; 32]),
                    vout: 0,
                },
                script_sig: bitcoin::ScriptBuf::new(),
                sequence: bitcoin::Sequence::MAX,
                witness: bitcoin::Witness::new(),
            }],
            output: vec![bitcoin::TxOut {
                value: bitcoin::Amount::from_sat(330),
                script_pubkey: bitcoin::ScriptBuf::new(),
            }],
        })
        .unwrap()
    }

    #[test]
    fn extracts_spent_outpoint() {
        let txid = "a".repeat(64);
        let body = format!("submit failed: {txid}:2 already spent");
        assert_eq!(
            extract_spent_outpoint(&body).as_deref(),
            Some(format!("{txid}:2").as_str())
        );
    }

    #[test]
    fn hard_coded_address_has_expected_script() {
        let address = ark_core::ArkAddress::decode(GAME_ADDRESS).unwrap();
        assert_eq!(
            address.to_p2tr_script_pubkey().to_hex_string(),
            "51201bb845d8a4812365639aed19b3000d73fee6a8ed9f9e5da2bd7adf2c08659d4a"
        );
    }

    #[test]
    fn registration_total_never_creates_sub_minimum_change() {
        assert!(valid_registration_total(660, 660, 330));
        assert!(!valid_registration_total(700, 660, 330));
        assert!(!valid_registration_total(989, 660, 330));
        assert!(valid_registration_total(990, 660, 330));
    }

    #[test]
    fn pending_journal_roundtrips_exact_psbts() {
        let signed_ark = test_psbt(1);
        let txid = signed_ark.unsigned_tx.compute_txid();
        let action = PendingAction::UnknownMove {
            submission: txbuild::UnknownSubmission {
                txid,
                signed_ark: signed_ark.clone(),
                checkpoints: vec![test_psbt(2)],
                last_error: "prepared".to_string(),
            },
            direction: crate::game::DIR_RIGHT,
            sequence: 7,
        };
        let raw = serde_json::to_string(&pending_action_to_journal(&action)).unwrap();
        let decoded: PendingJournalAction = serde_json::from_str(&raw).unwrap();
        let restored = pending_action_from_journal(decoded).unwrap();
        let PendingAction::UnknownMove {
            submission,
            direction,
            sequence,
        } = restored
        else {
            panic!("wrong restored pending action");
        };
        assert_eq!(submission.txid, txid);
        assert_eq!(submission.signed_ark.serialize(), signed_ark.serialize());
        assert_eq!(direction, crate::game::DIR_RIGHT);
        assert_eq!(sequence, 7);
    }
}
