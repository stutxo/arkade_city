//! Match orchestration: handshake, asset issuance, event tx sending,
//! polling, causal ordering, and deterministic replay.
//!
//! No server: the host's per-game Arkade address travels in the share link,
//! the joiner answers with a START tx, and from then on both clients watch
//! both players' VTXO scripts through the public indexer.
//!
//! Each match uses a FRESH keypair (new address per game), funded from the
//! master key with one plain offchain send. The master key is the funding
//! identity shown in the UI; the game key is the match identity in the link.

use crate::arkade::{ArkadeRest, ServerParams, VtxoRecord};
use crate::game::{Input, Phase as SimPhase, Sim};
use crate::gamelog::{order_events, payloads_from_tx, Event, Kind, Msg};
use crate::keys::Keys;
use crate::{gamelog, txbuild};
use anyhow::{anyhow, Result};
use ark_core::asset::AssetId;
use bitcoin::Txid;
use std::collections::HashSet;

pub const BULLET_SUPPLY: u64 = crate::game::START_AMMO as u64;
/// Sats moved from the master key into a fresh game key at match start.
pub const GAME_FUND_SATS: u64 = 5_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Host: waiting for a START tx to arrive.
    Hosting,
    /// Joiner/host: funding the game key from the master key.
    JoinFunding,
    /// Joiner: START sent, waiting for the host's ACK.
    JoinSent,
    /// Handshake done; issuing the bullet asset.
    Arming,
    Playing,
    Done { winner: usize, verified: bool },
}

pub struct MatchApp {
    /// The persistent funding identity (shown in the UI, funded externally).
    pub master: Keys,
    /// The active signing identity: the master key when idle, a fresh
    /// per-match key once hosting/joining.
    pub keys: Keys,
    pub rest: ArkadeRest,
    pub params: ServerParams,
    pub info: ark_core::server::Info,

    pub phase: Phase,
    pub match_tag: Option<[u8; 8]>,
    pub opponent_addr: Option<ark_core::ArkAddress>,

    /// My bullet asset (issued at match start).
    pub my_bullet_asset: Option<AssetId>,
    /// Opponent's bullet asset, first seen on their script (informational).
    pub opp_bullet_asset: Option<String>,

    seq: u32,
    last_sent_tag: [u8; 8],
    sent_txids: HashSet<Txid>,

    events: Vec<Event>,
    /// Virtual txids already parsed.
    seen_txs: HashSet<Txid>,

    pub sim: Option<Sim>,
    pub sim_inputs: Vec<Input>,

    /// A send is in flight (wasm is single-threaded; this serializes sends).
    pub sending: bool,
    /// Queued direction presses (dir bytes), packed into move txs.
    pub pending_dirs: Vec<u8>,
    pub pending_fires: u32,
    /// Outpoints the operator reports as spent by a pending tx we crashed
    /// mid-finalize; skipped when picking inputs.
    excluded_outpoints: HashSet<String>,
    /// Last poll error text surfaced to the log (dedupes repeats).
    last_poll_error: Option<String>,
    /// Wall-clock ms of the last successful poll (UI proof of life).
    last_sync_ms: u64,
    /// Balance cached from the latest poll, for the snapshot.
    balance_cache: u64,

    pub log: Vec<String>,
}

impl MatchApp {
    pub fn new(master: Keys, rest: ArkadeRest, params: ServerParams) -> Self {
        let info = txbuild::server_info(&params);
        let active = Keys::from_hex(&master.secret_hex()).expect("same key");
        Self {
            master,
            keys: active,
            rest,
            params,
            info,
            phase: Phase::Idle,
            match_tag: None,
            opponent_addr: None,
            my_bullet_asset: None,
            opp_bullet_asset: None,
            seq: 0,
            last_sent_tag: [0; 8],
            sent_txids: HashSet::new(),
            events: Vec::new(),
            seen_txs: HashSet::new(),
            sim: None,
            sim_inputs: Vec::new(),
            sending: false,
            pending_dirs: Vec::new(),
            pending_fires: 0,
            excluded_outpoints: HashSet::new(),
            last_poll_error: None,
            last_sync_ms: 0,
            balance_cache: 0,
            log: Vec::new(),
        }
    }

    fn in_match(&self) -> bool {
        self.game_key_hex().is_some()
    }

    /// The game key hex, if a per-match key is active.
    fn game_key_hex(&self) -> Option<String> {
        let active = self.keys.secret_hex();
        (active != self.master.secret_hex()).then_some(active)
    }

    fn new_game_key(&mut self) {
        self.keys = Keys::generate().expect("rng");
        self.log_line("new game key → fresh address for this match");
    }

    /// The address to show for funding: the master key's.
    pub fn funding_address(&self) -> ark_core::ArkAddress {
        txbuild::player_vtxo(&self.master, &self.params)
            .expect("vtxo")
            .to_ark_address()
    }

    /// The active (match) address: goes into links and START payloads.
    pub fn my_address(&self) -> ark_core::ArkAddress {
        txbuild::player_vtxo(&self.keys, &self.params)
            .expect("vtxo")
            .to_ark_address()
    }

    pub fn my_script_hex(&self) -> String {
        txbuild::player_vtxo(&self.keys, &self.params)
            .expect("vtxo")
            .script_pubkey()
            .to_hex_string()
    }

    fn master_script_hex(&self) -> String {
        txbuild::player_vtxo(&self.master, &self.params)
            .expect("vtxo")
            .script_pubkey()
            .to_hex_string()
    }

    fn log_line(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() > 200 {
            self.log.remove(0);
        }
    }

    async fn spendable_for(&self, keys: &Keys) -> Result<Vec<VtxoRecord>> {
        let script = txbuild::player_vtxo(keys, &self.params)?
            .script_pubkey()
            .to_hex_string();
        self.rest.get_vtxos(&script, "spendableOnly").await
    }

    /// My spendable VTXOs on the active script.
    pub async fn my_spendable(&self) -> Result<Vec<VtxoRecord>> {
        self.spendable_for(&self.keys).await
    }

    pub async fn balance_sats(&self) -> Result<u64> {
        let master: u64 = self
            .spendable_for(&self.master)
            .await?
            .iter()
            .map(|v| v.amount_sats)
            .sum();
        let game: u64 = if self.in_match() {
            self.my_spendable().await?.iter().map(|v| v.amount_sats).sum()
        } else {
            0
        };
        Ok(master + game)
    }

    /// Ensure the active game key holds funds; if not, move some from the
    /// master key with a plain offchain send. Returns true when funded.
    async fn ensure_game_funded(&mut self) -> Result<bool> {
        if !self.in_match() {
            return Ok(true);
        }
        if !self.my_spendable().await?.is_empty() {
            return Ok(true);
        }
        let master_spendable = self.spendable_for(&self.master).await?;
        let funding = master_spendable
            .iter()
            .filter(|v| !self.excluded_outpoints.contains(&v.outpoint.to_string()))
            .max_by_key(|v| v.amount_sats)
            .cloned()
            .ok_or_else(|| anyhow!("master wallet is empty — fund your address first"))?;
        let game_addr = self.my_address();
        let (ark_tx, checkpoints) = txbuild::build_send_tx(
            &self.master,
            &self.params,
            &self.info,
            &[funding],
            game_addr,
            GAME_FUND_SATS,
        )?;
        let txid = self.send_raw(ark_tx, checkpoints).await?;
        self.log_line(format!("funded game key with {GAME_FUND_SATS} sats (tx {txid})"));
        Ok(false) // funds visible on next poll
    }

    /// Become the host: fresh game key/address, then wait for a START.
    pub fn host_game(&mut self) {
        self.new_game_key();
        self.phase = Phase::Hosting;
        self.log_line("hosting: share your link, waiting for START tx");
    }

    /// Forget all match state (local only; the chains on the indexer are
    /// immutable). The game key is dropped; the master key is kept.
    pub fn reset_match(&mut self) {
        let master = Keys::from_hex(&self.master.secret_hex()).expect("same key");
        let rest = self.rest.clone();
        let params = self.params.clone();
        *self = MatchApp::new(master, rest, params);
        self.log_line("match state reset");
    }

    /// Join via the host's address: fresh game key, fund it, send START.
    /// Idempotent: if our chain history already contains a START paying this
    /// host (same game key), adopt it instead of sending a second one.
    pub async fn join_game(&mut self, host_address: &str) -> Result<()> {
        let host = ark_core::ArkAddress::decode(host_address)
            .map_err(|e| anyhow!("bad host address: {e}"))?;

        // Rejoining with an active match key? Otherwise start fresh.
        if !self.in_match() {
            self.new_game_key();
        }
        if host == self.my_address() {
            return Err(anyhow!(
                "that's your own address — open the invite link on the other player's device/browser"
            ));
        }
        if !self.ensure_game_funded().await? {
            self.phase = Phase::JoinFunding;
            self.opponent_addr = Some(host);
            self.log_line("funding game key; will send START when it lands");
            return Ok(());
        }
        if self.adopt_existing_start(host).await? {
            return Ok(());
        }
        self.send_start(host).await
    }

    /// Scan our own chain history for a START that pays this host's script.
    async fn adopt_existing_start(&mut self, host: ark_core::ArkAddress) -> Result<bool> {
        let my_addr = self.my_address().encode();
        let records = self.rest.get_vtxos(&self.my_script_hex(), "").await?;
        let txids: Vec<String> = records
            .iter()
            .filter_map(|r| r.ark_txid.clone())
            .filter(|t| t.len() == 64)
            .collect();
        if txids.is_empty() {
            return Ok(false);
        }
        let host_script = host.to_p2tr_script_pubkey();
        for psbt in self.rest.get_virtual_txs(&txids).await? {
            let tx = &psbt.unsigned_tx;
            if !tx.output.iter().any(|o| o.script_pubkey == host_script) {
                continue;
            }
            for payload in payloads_from_tx(tx) {
                let Some(msg) = Msg::decode(&payload) else { continue };
                if msg.kind == Kind::Start && msg.data == my_addr.as_bytes() {
                    let txid = tx.compute_txid();
                    self.match_tag = Some(gamelog::txid_tag(&txid));
                    self.opponent_addr = Some(host);
                    self.sent_txids.insert(txid);
                    self.phase = Phase::JoinSent;
                    self.log_line(format!("reusing earlier START (tx {txid}); waiting for ACK"));
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    async fn send_start(&mut self, host: ark_core::ArkAddress) -> Result<()> {
        let funding = self.chain_input().await?;
        // START: dust to the host + our game address in the OP_RETURN payload.
        // The match tag derives from the START txid once the tx exists.
        let msg = Msg {
            match_tag: [0; 8],
            seq: self.seq,
            prev: [0; 8],
            tick_ms: now_ms(),
            kind: Kind::Start,
            data: self.my_address().encode().into_bytes(),
        };
        let (ark_tx, checkpoints) = txbuild::build_message_tx(
            &self.keys,
            &self.params,
            &self.info,
            &[funding],
            host,
            self.params.dust_sats,
            &msg.encode(),
        )?;
        let txid = self.send_raw(ark_tx, checkpoints).await?;
        self.match_tag = Some(gamelog::txid_tag(&txid));
        self.opponent_addr = Some(host);
        self.phase = Phase::JoinSent;
        self.log_line(format!("START sent (tx {txid}); waiting for ACK"));
        Ok(())
    }

    /// Build, sign, submit, finalize; remember the txid as ours.
    async fn send_raw(
        &mut self,
        ark_tx: bitcoin::Psbt,
        checkpoints: Vec<bitcoin::Psbt>,
    ) -> Result<Txid> {
        self.sending = true;
        let result = txbuild::run_tx(&self.keys, &self.rest, ark_tx, checkpoints).await;
        self.sending = false;
        let txid = result?;
        self.sent_txids.insert(txid);
        self.last_sent_tag = gamelog::txid_tag(&txid);
        self.seq += 1;
        Ok(txid)
    }

    fn next_msg(&mut self, kind: Kind, data: Vec<u8>) -> Msg {
        Msg {
            match_tag: self.match_tag.unwrap_or([0; 8]),
            seq: self.seq,
            prev: self.last_sent_tag,
            tick_ms: now_ms(),
            kind,
            data,
        }
    }

    /// Queue direction presses (dir bytes) and fire presses.
    pub fn queue_inputs(&mut self, dirs: &[u8], fires: u32) {
        const MAX_QUEUE: usize = 8;
        for &d in dirs {
            if self.pending_dirs.len() < MAX_QUEUE {
                self.pending_dirs.push(d);
            }
        }
        if fires > 0 {
            self.pending_fires = (self.pending_fires + fires).min(4);
        }
    }

    /// Drive pending sends; at most one tx per step (they are serialized and
    /// each takes a network round trip).
    pub async fn flush_pending(&mut self) -> Result<()> {
        if self.sending || !matches!(self.phase, Phase::Playing) {
            return Ok(());
        }
        if self.pending_fires > 0 {
            self.pending_fires -= 1;
            if let Err(e) = self.send_fire().await {
                self.handle_send_error(&e);
            }
            return Ok(());
        }
        if !self.pending_dirs.is_empty() {
            // Pack up to 4 direction steps into one move tx.
            let n = self.pending_dirs.len().min(4);
            let dirs: Vec<u8> = self.pending_dirs.drain(..n).collect();
            if let Err(e) = self.send_move(&dirs).await {
                self.handle_send_error(&e);
            }
        }
        Ok(())
    }

    /// Log a send failure; if the operator says an input is held by a stale
    /// pending tx, exclude that outpoint so future sends pick another one.
    fn handle_send_error(&mut self, err: &anyhow::Error) {
        let text = format!("{err:#}");
        if let Some(op) = extract_spent_outpoint(&text) {
            if self.excluded_outpoints.insert(op.clone()) {
                self.log_line(format!("input {op} held by a pending tx; skipping it"));
            }
        }
        self.log_line(format!("send failed: {text}"));
    }

    async fn chain_input(&self) -> Result<VtxoRecord> {
        let spendable = self.my_spendable().await?;
        // Continue from the largest spendable output: the self-send change
        // output forms the chain naturally. Skip outpoints the server still
        // holds as spent by a pending tx we crashed out of.
        spendable
            .into_iter()
            .filter(|v| !self.excluded_outpoints.contains(&v.outpoint.to_string()))
            .max_by_key(|v| v.amount_sats)
            .ok_or_else(|| anyhow!("no spendable VTXO for event tx"))
    }

    async fn send_move(&mut self, dirs: &[u8]) -> Result<()> {
        let input = self.chain_input().await?;
        let msg = self.next_msg(Kind::Move, dirs.to_vec());
        let (ark_tx, checkpoints) =
            txbuild::build_event_tx(&self.keys, &self.params, &self.info, &input, &msg.encode())?;
        let txid = self.send_raw(ark_tx, checkpoints).await?;
        self.log_line(format!("move {:?} → {txid}", dirs));
        Ok(())
    }

    async fn send_fire(&mut self) -> Result<()> {
        let asset = self
            .my_bullet_asset
            .ok_or_else(|| anyhow!("bullet asset not issued yet"))?;
        let spendable = self.my_spendable().await?;
        let asset_id_str = asset.to_string();
        let Some(carrier) = spendable
            .iter()
            .find(|v| v.assets.iter().any(|(id, amt)| id == &asset_id_str && *amt > 0))
            .cloned()
        else {
            self.log_line("out of ammo (no bullet asset VTXO)");
            return Ok(());
        };
        // weapon id 0 = standard bullet; the byte also keeps the payload
        // off the 32-byte sub-dust VTXO script shape.
        let msg = self.next_msg(Kind::Fire, vec![0]);
        let (ark_tx, checkpoints) = txbuild::build_burn_tx(
            &self.keys,
            &self.params,
            &self.info,
            &[carrier],
            asset,
            1,
            &msg.encode(),
        )?;
        let txid = self.send_raw(ark_tx, checkpoints).await?;
        self.log_line(format!("fire! burn in {txid}"));
        Ok(())
    }

    async fn issue_bullets(&mut self) -> Result<()> {
        // Issuance pays ourselves dust for the asset output; avoid inputs
        // whose change would land in the invalid sub-dust band (0 < change < dust).
        let spendable = self.my_spendable().await?;
        let dust = self.params.dust_sats;
        let input = spendable
            .iter()
            .filter(|v| !self.excluded_outpoints.contains(&v.outpoint.to_string()))
            .filter(|v| v.amount_sats == dust || v.amount_sats >= 2 * dust)
            .max_by_key(|v| v.amount_sats)
            .cloned()
            .ok_or_else(|| {
                anyhow!("no issuance-compatible VTXO (need exactly {dust} or >= {} sats)", 2 * dust)
            })?;
        let metadata = vec![
            ("game".to_string(), "arkade-duel".to_string()),
            ("match".to_string(), hex8(&self.match_tag.unwrap_or([0; 8]))),
            ("kind".to_string(), "bullet:standard".to_string()),
        ];
        let (ark_tx, checkpoints, asset_id) = txbuild::build_issue_tx(
            &self.keys,
            &self.params,
            &self.info,
            &input,
            BULLET_SUPPLY,
            metadata,
        )?;
        let txid = self.send_raw(ark_tx, checkpoints).await?;
        self.my_bullet_asset = Some(asset_id);
        self.log_line(format!("issued {BULLET_SUPPLY} bullets, asset {asset_id} (tx {txid})"));
        Ok(())
    }

    async fn send_end(&mut self, state_hash: u64) -> Result<()> {
        let input = self.chain_input().await?;
        let msg = self.next_msg(Kind::End, state_hash.to_le_bytes().to_vec());
        let (ark_tx, checkpoints) =
            txbuild::build_event_tx(&self.keys, &self.params, &self.info, &input, &msg.encode())?;
        let txid = self.send_raw(ark_tx, checkpoints).await?;
        self.log_line(format!("END sent (state {state_hash:016x}) in {txid}"));
        Ok(())
    }

    /// Handle a one-shot UI command ("host" / "join"). Idempotent by phase.
    /// State-changing; callers should persist right after this returns.
    pub async fn handle_command(&mut self, command: &str, arg: &str) {
        match command {
            "host" if matches!(self.phase, Phase::Idle) => self.host_game(),
            "join" if matches!(self.phase, Phase::Idle | Phase::JoinFunding) => {
                if let Err(e) = self.join_game(arg).await {
                    self.log_line("join failed");
                    self.handle_send_error(&e);
                }
            }
            _ => {}
        }
    }

    /// The serialized driver tick: queue inputs, poll, advance.
    pub async fn step(&mut self, dirs: &[u8], fires: u32) -> Result<()> {
        self.queue_inputs(dirs, fires);

        if let Err(e) = self.refresh().await {
            let text = format!("{e:#}");
            if self.last_poll_error.as_deref() != Some(text.as_str()) {
                self.last_poll_error = Some(text.clone());
                self.log_line(format!("sync: {text}"));
            }
        } else {
            self.last_poll_error = None;
            self.last_sync_ms = now_ms();
        }

        self.balance_cache = self.balance_sats().await.unwrap_or(self.balance_cache);
        Ok(())
    }

    /// Poll indexer, ingest new txs, advance the state machine and sim.
    pub async fn refresh(&mut self) -> Result<()> {
        // 1. Collect new events from both scripts.
        self.collect_side().await?;
        if let Some(opp_script) = self.opponent_script() {
            self.collect_side_scripts(&[opp_script]).await?;
        }

        // 2. State machine transitions driven by what we ingested.
        match self.phase {
            Phase::Hosting => {
                if let Some((tag, joiner)) = self.find_start() {
                    self.adopt_start(tag, joiner).await?;
                }
            }
            Phase::JoinFunding => {
                // The join command ran before the game key was funded; retry.
                if let Some(host) = self.opponent_addr {
                    if let Err(e) = self.join_game(&host.encode()).await {
                        self.handle_send_error(&e);
                    }
                }
            }
            Phase::JoinSent => {
                if self.find_ack() {
                    self.begin_match().await?;
                }
            }
            Phase::Arming => {
                if self.my_bullet_asset.is_none() && !self.sending {
                    if let Err(e) = self.issue_bullets().await {
                        self.handle_send_error(&e);
                    }
                } else if self.my_bullet_asset.is_some() {
                    self.phase = Phase::Playing;
                    self.log_line("match live — WASD steps, space fires");
                }
            }
            Phase::Playing => {
                self.flush_pending().await?;
            }
            _ => {}
        }

        // 3. Feed the sim and detect the end.
        self.advance_sim().await?;
        Ok(())
    }

    fn opponent_script(&self) -> Option<String> {
        let addr = self.opponent_addr?;
        Some(addr.to_p2tr_script_pubkey().to_hex_string())
    }

    async fn collect_side(&mut self) -> Result<()> {
        // While hosting, watch the game key's script (START lands there).
        let scripts = [self.my_script_hex()];
        self.collect_side_scripts(&scripts).await
    }

    /// Ingest new game txs visible on one side's scripts.
    async fn collect_side_scripts(&mut self, scripts: &[String]) -> Result<()> {
        let mut records = Vec::new();
        for s in scripts {
            records.extend(self.rest.get_vtxos(s, "").await?);
        }
        // New creating txids = candidate event txs. The indexer's arkTxid
        // field is empty for some records (fresh preconfirmed outputs), so
        // fall back to the outpoint txid: for offchain-created VTXOs that IS
        // the creating virtual tx. Batch/boarding records may resolve to
        // non-virtual txids; those simply fail to fetch and are skipped.
        let mut new_txids: Vec<String> = Vec::new();
        for r in &records {
            let candidate = r
                .ark_txid
                .as_deref()
                .filter(|t| t.len() == 64)
                .map(str::to_string)
                .unwrap_or_else(|| r.outpoint.txid.to_string());
            let Ok(parsed) = candidate.parse::<Txid>() else { continue };
            if !self.seen_txs.contains(&parsed) {
                self.seen_txs.insert(parsed);
                new_txids.push(candidate);
            }
        }
        if new_txids.is_empty() {
            return Ok(());
        }
        // Fetch in one batch; if any txid isn't a virtual tx (e.g. a batch
        // commitment from the outpoint fallback), retry one-by-one so a
        // single bad id doesn't hide real events.
        let txs = match self.rest.get_virtual_txs(&new_txids).await {
            Ok(txs) => txs,
            Err(_) => {
                let mut acc = Vec::new();
                for id in &new_txids {
                    if let Ok(mut txs) = self.rest.get_virtual_txs(&[id.clone()]).await {
                        acc.append(&mut txs);
                    }
                }
                acc
            }
        };
        let my_pk = self.keys.owner_pk();
        let my_pk_bytes = my_pk.serialize();
        let master_pk_bytes = self.master.owner_pk().serialize();
        for psbt in &txs {
            let tx = &psbt.unsigned_tx;
            let txid = tx.compute_txid();
            let has_asset_packet = tx
                .output
                .iter()
                .any(|o| ark_core::extension::is_extension(&o.script_pubkey));
            // Attribute by input ownership: the signer's x-only pubkey is in
            // the PSBT's tap_script_sigs keys (and/or final witness). The
            // master key also counts as "us" (game-key funding sends).
            let signed_by = |pk: &[u8; 32]| {
                psbt.inputs.iter().any(|inp| {
                    inp.tap_script_sigs.keys().any(|(k, _)| k.serialize() == *pk)
                        || inp.final_script_witness.as_ref().is_some_and(|w| {
                            w.iter().any(|e| e.windows(32).any(|x| x == pk))
                        })
                })
            };
            let mine =
                signed_by(&my_pk_bytes) || signed_by(&master_pk_bytes) || self.sent_txids.contains(&txid);
            let side = if mine { 0 } else { 1 };
            for payload in payloads_from_tx(tx) {
                let Some(msg) = Msg::decode(&payload) else { continue };
                // Once a match is adopted, ignore stray messages for others.
                if let Some(tag) = self.match_tag {
                    if msg.kind != Kind::Start && msg.match_tag != tag {
                        continue;
                    }
                }
                self.events.push(Event {
                    txid,
                    side,
                    has_asset_packet,
                    msg,
                });
            }
        }
        Ok(())
    }

    /// Host: look for a START event among ingested txs.
    fn find_start(&self) -> Option<([u8; 8], ark_core::ArkAddress)> {
        for e in &self.events {
            if e.msg.kind != Kind::Start || e.side != 1 {
                continue;
            }
            let Ok(text) = String::from_utf8(e.msg.data.clone()) else {
                continue;
            };
            let Ok(addr) = ark_core::ArkAddress::decode(text.trim()) else {
                continue;
            };
            return Some((gamelog::txid_tag(&e.txid), addr));
        }
        None
    }

    async fn adopt_start(&mut self, tag: [u8; 8], joiner: ark_core::ArkAddress) -> Result<()> {
        self.match_tag = Some(tag);
        self.opponent_addr = Some(joiner);
        if !self.ensure_game_funded().await? {
            self.log_line("START received; funding game key before ACK");
            return Ok(()); // retry on next poll
        }
        let msg = Msg {
            match_tag: tag,
            seq: self.seq,
            prev: self.last_sent_tag,
            tick_ms: now_ms(),
            kind: Kind::Ack,
            data: vec![],
        };
        let funding = self.chain_input().await?;
        let (ark_tx, checkpoints) = txbuild::build_message_tx(
            &self.keys,
            &self.params,
            &self.info,
            &[funding],
            joiner,
            self.params.dust_sats,
            &msg.encode(),
        )?;
        self.send_raw(ark_tx, checkpoints).await?;
        self.log_line("START received; ACK sent");
        self.begin_match().await
    }

    /// Joiner: the host's ACK for any START we ever sent (a crashed first
    /// attempt may have finalized without our local state advancing).
    fn find_ack(&mut self) -> bool {
        let mut candidates: Vec<[u8; 8]> = self.match_tag.into_iter().collect();
        let my_addr = self.my_address().encode();
        for e in &self.events {
            if e.msg.kind == Kind::Start
                && e.side == 0
                && String::from_utf8(e.msg.data.clone()).ok().as_deref() == Some(my_addr.as_str())
            {
                candidates.push(gamelog::txid_tag(&e.txid));
            }
        }
        for e in &self.events {
            if e.msg.kind != Kind::Ack || e.side != 1 {
                continue;
            }
            if !candidates.contains(&e.msg.match_tag) {
                continue;
            }
            self.match_tag = Some(e.msg.match_tag);
            return true;
        }
        false
    }

    async fn begin_match(&mut self) -> Result<()> {
        self.sim = Some(Sim::new());
        self.phase = Phase::Arming;
        self.log_line("handshake complete; arming…");
        Ok(())
    }

    /// Rebuild sim inputs from the ordered log and run them.
    fn rebuild_sim_inputs(&self) -> Vec<Input> {
        let Some(tag) = self.match_tag else { return Vec::new() };
        let events: Vec<Event> = self
            .events
            .iter()
            .filter(|e| e.msg.match_tag == tag)
            .cloned()
            .collect();
        let ordered = order_events(events);
        let mut inputs = Vec::new();
        for e in &ordered {
            match e.msg.kind {
                Kind::Move => {
                    for &dir in &e.msg.data {
                        inputs.push(Input::Move {
                            side: e.side as usize,
                            dir,
                        });
                    }
                }
                Kind::Fire => inputs.push(Input::Fire {
                    side: e.side as usize,
                }),
                _ => {}
            }
        }
        inputs
    }

    async fn advance_sim(&mut self) -> Result<()> {
        if self.sim.is_none() {
            return Ok(());
        }
        let inputs = self.rebuild_sim_inputs();
        self.sim_inputs = inputs.clone();
        // Re-run from scratch: cheap at this scale, immune to late events.
        let mut fresh = Sim::new();
        fresh.run(&inputs);
        if let SimPhase::Done { winner } = fresh.phase {
            if matches!(self.phase, Phase::Playing) {
                self.phase = Phase::Done {
                    winner,
                    verified: false,
                };
                let hash = fresh.state_hash();
                self.log_line(format!("match over: player {winner} wins; sending END"));
                // Best-effort END; failure leaves the match recorded anyway.
                if !self.sending {
                    let _ = self.send_end(hash).await;
                }
            }
        }
        // Verify against the opponent's END: matching state hashes mean both
        // sides replayed to the identical outcome.
        if let Phase::Done { winner, verified } = self.phase {
            if !verified {
                let my_hash = Some(fresh.state_hash());
                let ordered: Vec<Event> = self
                    .events
                    .iter()
                    .filter(|e| Some(e.msg.match_tag) == self.match_tag)
                    .cloned()
                    .collect();
                let opp_end_ok = ordered.iter().any(|e| {
                    e.side == 1
                        && e.msg.kind == Kind::End
                        && e.msg.data.len() == 8
                        && Some(u64::from_le_bytes(e.msg.data[..8].try_into().unwrap()))
                            == my_hash
                });
                if opp_end_ok {
                    self.phase = Phase::Done {
                        winner,
                        verified: true,
                    };
                    self.log_line("opponent END matches local replay — verified");
                }
            }
        }
        self.sim = Some(fresh);
        Ok(())
    }

    pub fn snapshot(&self, version: &str) -> Snapshot {
        let (players, bullets, ammo, sim_phase) = match &self.sim {
            Some(sim) => {
                let phase = match sim.phase {
                    SimPhase::Playing => "playing",
                    SimPhase::Done { .. } => "done",
                };
                (
                    Some(sim.pos.map(|(x, y)| [x, y])),
                    sim.bullets.iter().map(|b| [b.x, b.y]).collect::<Vec<_>>(),
                    Some(sim.ammo),
                    phase,
                )
            }
            None => (None, Vec::new(), None, "idle"),
        };
        let (phase, winner, verified) = match self.phase {
            Phase::Idle => ("idle", None, false),
            Phase::Hosting => ("hosting", None, false),
            Phase::JoinFunding => ("join-funding", None, false),
            Phase::JoinSent => ("join-sent", None, false),
            Phase::Arming => ("arming", None, false),
            Phase::Playing => ("playing", None, false),
            Phase::Done { winner, verified } => ("done", Some(winner), verified),
        };
        Snapshot {
            network: match self.params.network {
                bitcoin::Network::Bitcoin => "mainnet".to_string(),
                _ => "signet".to_string(),
            },
            version: version.to_string(),
            phase: phase.to_string(),
            sim_phase: sim_phase.to_string(),
            address: self.funding_address().encode(),
            game_address: self.in_match().then(|| self.my_address().encode()),
            opponent: self.opponent_addr.map(|a| a.encode()),
            match_id: self.match_tag.map(|t| hex8(&t)),
            players,
            bullets,
            ammo,
            winner,
            verified,
            my_bullet_asset: self.my_bullet_asset.map(|a| a.to_string()),
            sending: self.sending,
            balance: self.balance_cache,
            events: self.events.len() as u32,
            last_sync_ms: self.last_sync_ms,
            log: self.log.clone(),
        }
    }
}

/// UI snapshot. Plain serde struct so serde-wasm-bindgen produces a real JS
/// object (serializing a serde_json::Value yields a JS Map instead — every
/// field reads as undefined).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub network: String,
    pub version: String,
    pub phase: String,
    pub sim_phase: String,
    /// Master-key address: the funding target shown in the UI.
    pub address: String,
    /// Per-match address (present while a match is active).
    pub game_address: Option<String>,
    pub opponent: Option<String>,
    pub match_id: Option<String>,
    pub players: Option<[[i32; 2]; 2]>,
    pub bullets: Vec<[i32; 2]>,
    pub ammo: Option<[u32; 2]>,
    pub winner: Option<usize>,
    pub verified: bool,
    pub my_bullet_asset: Option<String>,
    pub sending: bool,
    pub balance: u64,
    pub events: u32,
    pub last_sync_ms: u64,
    pub log: Vec<String>,
}

pub fn hex8(tag: &[u8; 8]) -> String {
    use bitcoin::hex::DisplayHex;
    tag.to_lower_hex_string()
}

impl MatchApp {
    /// Serializable match state for reload recovery. The event log itself is
    /// NOT persisted: on reload it is re-ingested from the indexer (the
    /// chains are the source of truth).
    pub fn export_state(&self) -> serde_json::Value {
        let phase = match self.phase {
            Phase::Idle => "idle",
            Phase::Hosting => "hosting",
            Phase::JoinFunding => "join-funding",
            Phase::JoinSent => "join-sent",
            Phase::Arming => "arming",
            Phase::Playing => "playing",
            // Done replays to Done from the log on restore.
            Phase::Done { .. } => "playing",
        };
        serde_json::json!({
            "phase": phase,
            "gameKey": self.game_key_hex(),
            "matchTag": self.match_tag.map(|t| hex8(&t)),
            "opponent": self.opponent_addr.map(|a| a.encode()),
            "myBulletAsset": self.my_bullet_asset.map(|a| a.to_string()),
            "seq": self.seq,
            "sent": self.sent_txids.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
        })
    }

    pub fn import_state(&mut self, v: &serde_json::Value) {
        if let Some(key) = v.get("gameKey").and_then(|k| k.as_str()) {
            if let Ok(keys) = Keys::from_hex(key) {
                self.keys = keys;
            }
        }
        let phase = v.get("phase").and_then(|p| p.as_str()).unwrap_or("idle");
        self.phase = match phase {
            "hosting" => Phase::Hosting,
            "join-funding" => Phase::JoinFunding,
            "join-sent" => Phase::JoinSent,
            "arming" => Phase::Arming,
            "playing" => Phase::Playing,
            _ => Phase::Idle,
        };
        if let Some(tag) = v.get("matchTag").and_then(|t| t.as_str()) {
            let decoded: Result<Vec<u8>, _> = bitcoin::hex::FromHex::from_hex(tag);
            if let Ok(bytes) = decoded {
                if bytes.len() == 8 {
                    self.match_tag = Some(bytes[..8].try_into().unwrap());
                }
            }
        }
        if let Some(addr) = v.get("opponent").and_then(|a| a.as_str()) {
            self.opponent_addr = ark_core::ArkAddress::decode(addr).ok();
        }
        if let Some(asset) = v.get("myBulletAsset").and_then(|a| a.as_str()) {
            self.my_bullet_asset = asset.parse().ok();
        }
        if let Some(seq) = v.get("seq").and_then(|s| s.as_u64()) {
            self.seq = seq as u32;
        }
        if let Some(sent) = v.get("sent").and_then(|s| s.as_array()) {
            for t in sent.iter().filter_map(|t| t.as_str()) {
                if let Ok(txid) = t.parse() {
                    self.sent_txids.insert(txid);
                }
            }
        }
        if self.phase != Phase::Idle {
            self.log_line("restored match state from local storage");
        }
        // A restored in-progress match gets a fresh sim; the log replays it.
        if matches!(self.phase, Phase::Arming | Phase::Playing) {
            self.sim = Some(Sim::new());
        }
    }
}

/// Pull a "txid:vout already spent" outpoint out of an operator error body.
pub fn extract_spent_outpoint(text: &str) -> Option<String> {
    let idx = text.find("already spent")?;
    let before = &text[..idx];
    let token = before
        .split(|c: char| !c.is_ascii_hexdigit() && c != ':' && c != '.')
        .filter(|t| !t.is_empty())
        .last()?;
    let (txid, vout) = token.split_once(':')?;
    if txid.len() == 64 && txid.chars().all(|c| c.is_ascii_hexdigit()) && vout.parse::<u32>().is_ok() {
        Some(token.to_string())
    } else {
        None
    }
}

/// Wall-clock unix milliseconds (browser clock).
pub fn now_ms() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now() as u64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn extracts_spent_outpoint() {
        let txid = "a".repeat(64);
        let body = format!("submit failed (400): {{\"message\":\"{txid}:2 already spent\"}}");
        assert_eq!(
            super::extract_spent_outpoint(&body).as_deref(),
            Some(format!("{txid}:2").as_str())
        );
        assert_eq!(super::extract_spent_outpoint("some other error"), None);
    }
}
