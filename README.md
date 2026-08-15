# Arkade City

Arkade City is an endless multiplayer map reconstructed entirely from Arkade
transactions. Every browser runs the same static Rust/WASM app. There are no
matches, peers, relays, game servers, or operator changes.

Play the Mutinynet build at <https://stutxo.github.io/arkade_city/>.

```text
player browsers
  keys + transaction builder + maze replay
             |
             v
      Arkade Mutinynet
             |
             +-- one NUMS-owned player registry
             |
             `-- registered player VTXO scripts
                    containing native asset burns
```

The hard-coded registry address is:

```text
tark1qqcpq7yq3e8hhsx6ml3fud93m7827qggaurtzu3zwsr4a0qs0gf85xacghv2fqfrv43e4mgekvqq6ul7u65wm8u7tk3t67kl9syxt822nw9wzn
```

Its P2TR script is:

```text
51201bb845d8a4812365639aed19b3000d73fee6a8ed9f9e5da2bd7adf2c08659d4a
```

The address is a standard Arkade VTXO whose owner is Arkade's BIP341 NUMS
key. Its virtual spending paths require the unknown NUMS private key. Each new
player sends one 330-sat registration output there. The operator may sweep its
backing sats after expiry. Anyone can recompute it with:

```sh
cargo run --example burnaddr
```

## How Play Works

1. Opening the page creates an Arkade wallet and stores its key in browser
   storage. Returning to Mutinynet restores that wallet and dot.
2. Fund the displayed wallet with at least 660 sats over Arkade.
3. Once the wallet is funded, click **Enter game**. The app creates one
   registration transaction with a 330-sat registry marker and a 330-sat
   player carrier holding 50 W, 50 A, 50 S, and 50 D assets.
4. Never manually send funds to the registry address. Only fund the player
   address displayed by the app.
5. Each keypress protocol-burns one unit of the matching asset and recreates
   the same 330-sat carrier on the player's script.
6. Every client reads the registry, batches queries for all registered player
   scripts, validates their burns, and replays the maze.
7. Reaching the right-side exit records one lap and returns the dot to the
   entrance. The shared board never ends.

The 660-sat minimum is one 330-sat registration plus one reusable 330-sat
carrier. A move does not transfer sats to the registry. The carrier remains in
the player's wallet across all 200 moves and can be recovered afterward,
subject to Arkade expiry and any future operator fees.

Registration inputs must total exactly 660 sats or leave change accepted by
the operator's VTXO minimum. Mutinynet permits one-sat VTXOs, so change below
330 sats is encoded as a recoverable sub-dust output. Prefer funding amounts
that leave at least 330 sats of change when possible.

The wallet key and pending-transaction journal are stored in `localStorage`.
A recovery bundle with the raw key, address, signer, exit delay, and pending
journal is also shown in the UI. Use "Forget wallet" only after recovering or
sweeping any remaining funds. Browser storage is plaintext origin storage, so
treat the browser profile and every script served on the same origin as
wallet-sensitive.

Transactions cross a durable boundary before submission. Rust first signs the
exact Ark and checkpoint PSBTs locally. `web.js` writes and verifies that
journal in browser storage; only the following tick may query
`/v1/tx/pending`, submit, or finalize it. Reloading during registration, a move,
or a sweep therefore retries the same txid instead of rebuilding it.

## Registration Protocol

Each player's four immutable assets share one issuance txid, which is also the
player ID. Group indexes are:

The on-chain `arkade-maze-v2` identifier is retained so the renamed app can
continue replaying existing registrations and moves.

| Group | Asset metadata | Direction |
| --- | --- | --- |
| `0` | `game=arkade-maze-v2`, `move=w` | up |
| `1` | `game=arkade-maze-v2`, `move=d` | right |
| `2` | `game=arkade-maze-v2`, `move=s` | down |
| `3` | `game=arkade-maze-v2`, `move=a` | left |

A canonical registration transaction contains:

```text
output 0: 330 sats -> NUMS registry
output 1: 330 sats + 50 of each move asset -> player script
change:   optional output at least equal to the VTXO minimum -> player script
packet:   one immutable issuance group per direction
anchor:   final output
```

Clients require exactly one registry output at index zero, exactly one player
asset carrier at index one, four non-reissuable groups, canonical metadata,
and exactly 50 units assigned to the player carrier. Other outputs on the
registry address are ignored.

## Move Protocol

A canonical move transaction contains:

```text
input:       current 330-sat player carrier
output 0:    same 330-sat carrier -> registered player script
OP_RETURN 1: Arkade Asset V1 extension packet
OP_RETURN 2: 41 4d | 02 | sequence(u32 little endian)
anchor:      final output
```

The packet consumes the carrier's assets, preserves every unburned unit on
output zero, and has an input-minus-output deficit of exactly one unit for one
direction group. This is Arkade's native asset-burn mechanism, so indexed
supply decreases while the BTC carrier returns to the player.

The burned asset identifies the player by issuance txid and direction by
group index. The receipt only orders that player's moves. Players do not
collide, so no global order is needed. Duplicate sequence numbers are resolved
by txid, and replay stops at the first sequence gap.

## Discovery

A fresh client performs two existing indexer queries:

1. Paginate every VTXO for the registry script and validate registration
   transactions.
2. Query the discovered player scripts in batches using repeated `scripts`
   query parameters, fetch each creating virtual transaction, and accept only
   canonical one-unit burns.

This provides complete historical replay without an asset-event endpoint.
Unresolved transaction fetches are retried. Active clients stop pagination at
known pages and periodically perform a full scan to tolerate shifting page
boundaries. Controls remain disabled until the local player's script has been
fully scanned and every local record has been resolved, preventing stale
sequence numbers from irreversibly burning assets.

## Build And Run

Rust 1.86+, `wasm-pack`, the `wasm32-unknown-unknown` target, and a clang with
wasm32 support are required.

```sh
./build.sh
python3 -m http.server 8000
```

Open `http://localhost:8000`. All copies use the same hard-coded Mutinynet
operator and registry.

To fund the displayed `tark1...` address, open
`https://faucet.mutinynet.com` and use its separate **Send to Arkade** form.
Do not paste an Arkade address into the faucet's on-chain destination field.

## Tests

```sh
cargo test --lib
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo run --example layout
cargo run --example smoke
./build.sh
```

## Repository Layout

| Path | Purpose |
| --- | --- |
| `src/lib.rs` | Mutinynet-pinned WASM API, restoration, and journal export |
| `src/match_.rs` | durable wallet actions, registry polling, lifecycle, and replay |
| `src/game.rs` | deterministic square maze and endless lap rules |
| `src/gamelog.rs` | registration, burn, receipt, and sequence validation |
| `src/txbuild.rs` | registration issuance, protocol burns, signing, and finalization |
| `src/arkade.rs` | paginated and batched Arkade REST/indexer client |
| `index.html`, `web.js`, `game.css` | static browser interface |

## Limits

- This is alpha software on Mutinynet. Registration irreversibly removes
  330 sats from the player's wallet; moves do not.
- History grows with both players and moves. A fresh client reads every
  registration and every registered player's VTXO history.
- Anyone can create a canonical registration and appear as a new player. There
  is no admission control, identity system, or anti-spam layer.
- The operator validates asset conservation and transactions, not maze rules.
  Every browser applies maze rules locally.
- There is no boarding, settlement, renewal, or unilateral exit flow in this
  browser client.
- Lost submit responses are recovered through the operator's existing
  `/v1/tx/pending` ownership proof and the identical transaction is resubmitted
  only when no pending copy exists. The browser persists the pending journal so
  a reload can resume recovery or finalization.
- Sweep sends every safely collaborative-spendable sat and asset to another
  Mutinynet Arkade address. Boarding, settlement, renewal, unilateral exit, and
  recovery of swept or sub-dust VTXOs still require a full Arkade wallet.
