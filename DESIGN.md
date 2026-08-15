# Design: Arkade Duel — serverless P2P game with an on-chain input log

Status: implemented (WASM build passes, native unit tests pass; live match
testing pending funded wallets). Default network: **Bitcoin mainnet** via the
public operator `https://arkade.computer` (override with `?server=`).

## Architecture

No game server exists. The whole game is a static Rust/WASM page (the same
pattern as silentpay.me): the deterministic simulation, the Arkade protocol
client, and the player's keys all run in the browser.

```
host browser (WASM)                         joiner browser (WASM)
  keys + sim + REST client                    keys + sim + REST client
        │                                          │
        └──────► Arkade Service (arkd) ◄───────────┘
              REST: /v1/info, /v1/indexer/*, /v1/tx/submit, /v1/tx/finalize
```

- **Keys**: per-player secp256k1 keypair generated in the browser, persisted
  in localStorage, exportable as hex. Mutinynet-only for now.
- **Tx building/signing**: `ark-core` (pinned `adcb5a8…`) compiled to WASM —
  `build_offchain_transactions`, `build_self_asset_issuance_transactions`,
  `build_asset_burn_transactions`, `sign_ark_transaction`,
  `sign_checkpoint_transaction`.
- **Transport**: plain HTTPS REST against the operator. CORS verified open
  (`access-control-allow-origin: *`). Polling (~1.5 s) replaces gRPC streams.
- **Reads**: the indexer tracks every VTXO by script. Both clients poll both
  players' P2TR scripts (the indexer's script queries reject non-P2TR
  scripts, so sub-dust forms are not watched; all messages use dust-sized
  P2TR outputs) and fetch full virtual txs by txid (`get_virtual_txs`) to
  read OP_RETURN payloads.

## Game protocol

The link *is* the host's Arkade address (`https://<site>/#tark1…`). The
match id is the START txid's 8-byte tag.

1. **START** (joiner → host): dust send to the host address + OP_RETURN
   payload carrying the joiner's own address. The host now knows where to
   answer.
2. **ACK** (host → joiner): dust send + OP_RETURN with the match start
   timestamp (host clock + 5 s grace). Both sides derive the same sim start
   tick from it.
3. **Arming**: each side self-issues `BULLET_SUPPLY` (20) units of their
   bullet asset (`build_self_asset_issuance_transactions`, metadata
   `{game, match, kind}`). The asset ID encodes `(issuance txid, group)` —
   unique per player per match.
4. **Moves**: chained self-send (dust back to self) + plain OP_RETURN
   `{tick, seq, prev, keymask}`. Logged on key *change*, never per tick.
5. **Shots**: burn 1 bullet unit (`build_asset_burn_transactions`) + the same
   OP_RETURN header. Burn is operator-validated, so ammo is enforced by
   asset conservation — you cannot fire what you don't hold. The OP_RETURN
   carries the game input; the extension packet is the proof.
6. **End**: the client whose replay finishes first sends `END {state_hash}`.
   The match is *verified* when both sides' END hashes match.

### Why messages carry 330 sats

The OP_RETURN payload itself is always zero-value and free; the dust output
is forced by operator validation (arkd v0.9.15 `validateOffchainTxOutputs`):

- OP_RETURN outputs must be zero-value; inputs must equal outputs, so the
  spent sats must land on a value output.
- Value outputs must be P2TR with `>= max(dust, vtxoMin)` = 330 sats on
  mainnet. Sub-dust OP_RETURN VTXOs are impossible on mainnet (the allowed
  band `< dust && >= vtxoMin` is empty) and unwatchable on Mutinynet (the
  indexer rejects non-P2TR script queries).
- Moves/fires are self-sends, so the 330 sats returns as the next chain
  link — zero net cost. Only START/ACK pay the counterparty (330 sats each,
  once per match), because a message is only visible on the recipient's
  script and the handshake exists to announce the joiner's address.

### Message wire format

```text
OP_RETURN "GM" ver(1) match(8) seq(4 LE) prev(8) tick(8 LE ms) kind(1) data(n)
```

`kind`: 0=START, 1=ACK, 2=MOVE (data = 1-byte WASD mask), 3=FIRE, 4=END
(data = 8-byte FNV-1a state hash). `match`/`prev` are the first 8 bytes of
the respective txid's internal bytes. Plain (non-extension) OP_RETURN outputs
must be zero-value and count toward the operator's `maxOpReturnOutputs`
(currently 3 — a burn tx uses 2: asset packet + header).

### Ordering without a server

Two players = two independent chains. Deterministic merge: per-side sequence
numbers preserve each chain's internal order; the interleave sorts by
`(tick, txid)`. Both clients compute the identical order from the same event
set (see `order_events`, property-tested in `gamelog::tests`).

### Determinism

The sim (`game.rs`) uses integer math only. Movement is a WASD bitmask;
facing derives from the last nonzero movement direction, so a FIRE event
needs no aim data — the replay reconstructs position and facing at the fire
tick. One hit wins. Late-arriving events trigger a full re-run from the
start tick (cheap at match scale).

## Operator constraints (validated against arkd source + live GetInfo)

- Offchain txs may carry plain zero-value OP_RETURN outputs; one ARK
  extension output; sub-dust OP_RETURN VTXOs; exactly one anchor output.
  (`validateOffchainTxOutputs` in `internal/core/application/service.go`)
 - Live mainnet params: dust 330, vtxo min **330** (sub-dust disabled),
  max 0.5 BTC per VTXO, `maxOpReturnOutputs` **2** (burn tx uses exactly 2:
  asset packet + header), `maxTxWeight` 40000, offchain fees 0, 200 sats per
  onchain batch output (settlement only), batch interval 60 s, unilateral
  exit delay ~7 days.
- The operator validates transaction shape and asset conservation — never
  game rules. Game-rule enforcement happens at replay.

## Trust & privacy

- All game messages are publicly readable from the indexer. That is the
  point (anyone can verify a match), but never put secrets in payloads.
- Preconfirmation trusts the operator not to double-sign; the TEE signer
  constrains this. Matches are short and stake-less, so exposure is dust.
- Keys in browser storage are a deliberate regression from the old native
  wallet's stance; mainnet use means real BTC behind a browser key — keep
  amounts tiny and defund after playing.

## Deferred / known gaps

- **Boarding** (onchain → VTXO) is not implemented in the WASM client:
  `register_intent` + the batch tree-signing protocol over REST. Fund the
  browser address by receiving from another Arkade wallet instead.
- **Settlement/renewal/exit** are out of scope; long-lived chains accrue
  unilateral-exit cost. Settle periodically with a full wallet.
- **Input latency**: poll interval + preconfirmation means opponent inputs
  land ~1.5–3 s late. The sim replays canonically at END; live view is
  optimistic.
- **Fees**: intent fee / tx fee rate are 0 on Mutinynet today. A nonzero fee
  regime needs change-aware chaining (the event chain currently assumes
  fee-free 330-sat self-sends).
- Mainnet would need: real key custody, fee handling, settle/renew lifecycle.
