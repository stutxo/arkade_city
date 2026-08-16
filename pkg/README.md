# Arkade City

Arkade City v3 is a finite multiplayer combat arena reconstructed entirely
from Arkade transactions. Every browser runs the same static Rust/WASM replay.
There is no game server, lobby, host, relay, peer connection, or modified
coordinator.

The Mutinynet build uses the existing operator-specific NUMS registry:

```text
tark1qqcpq7yq3e8hhsx6ml3fud93m7827qggaurtzu3zwsr4a0qs0gf85xacghv2fqfrv43e4mgekvqq6ul7u65wm8u7tk3t67kl9syxt822nw9wzn
```

Its P2TR script is
`51201bb845d8a4812365639aed19b3000d73fee6a8ed9f9e5da2bd7adf2c08659d4a`.
The owner is Arkade's BIP341 NUMS key. The operator may sweep registration
backing sats after expiry. `cargo run --example burnaddr` recomputes the address.

## Playing

1. Open the page. A wallet key is generated and stored in origin-local browser
   storage.
2. Fund the displayed Arkade address with at least 660 sats.
3. Click **Enter game**. The app creates a 330-sat registry output and a
   reusable 330-sat player carrier.
4. Choose an action whenever the previous carrier is indexed and available:
   W/A/S/D to move and face, Space to shoot, or R to revive.
5. Sweep remaining sats and assets before forgetting a wallet.

Every player starts with 3 HP, faces right, and spawns deterministically from
the issuance txid onto an open cell. Indexed actions resolve sequentially. A
shot removes 1 HP, and a transition to zero awards the shooter a kill. A miss
stops at the first wall, house, or boundary.
Overlapping targets are resolved by
the lexicographically lowest player ID. Dead movement and shooting are
no-ops after the asset is burned. Revive preserves kills, restores 3 HP at the
deterministic spawn, and faces right. Players may overlap. The leaderboard sorts
by kills descending, then player ID ascending.

## V3 Protocol

The fresh protocol identifier is `arkade-arena-v3`. V2 registrations and
assets at the same registry are ignored. A canonical registration issues six
immutable groups to output 1:

| Group | Metadata action | Supply | Meaning |
| --- | --- | ---: | --- |
| 0 | `w` | 50 | move/facing up |
| 1 | `d` | 50 | move/facing right |
| 2 | `s` | 50 | move/facing down |
| 3 | `a` | 50 | move/facing left |
| 4 | `bullet` | 50 | shoot |
| 5 | `life` | 5 | revive |

Each group has exact metadata `game=arkade-arena-v3` and `action=<name>`, no
control asset, no inputs, and one assignment of its exact supply to player
output 1. The transaction structure remains:

```text
output 0: 330 sats -> operator-specific NUMS registry
output 1: 330 sats + all six asset groups -> player script
change:   optional operator-valid BTC-only output -> player script
packet:   one canonical immutable issuance containing all six groups
```

An action transaction burns exactly one unit of its corresponding group,
preserves every other unit on output 0 of the recreated 330-sat player carrier,
and carries one `AM` v3 receipt with a little-endian `u32` player sequence.
Sequence starts at zero. Duplicate sequence claims resolve by indexed
`(createdAt, txid)` along the checkpoint-aware predecessor chain, and replay
stops at the first gap. This carrier chain always retains every canonical burn
for inventory and next-sequence continuity, including gameplay no-ops.

## Discovery And Ordering

Clients paginate the registry without trusting API page order, validate v3
registrations, query all registered player scripts, and validate each creating
virtual transaction. Accepted events are first reduced to each player's
canonical contiguous sequence and predecessor stream. Every canonical action
with a parseable timestamp enters gameplay exactly once. Actions are sorted
globally by `(createdAt, txid)` and applied one at a time. Movement updates
facing and then advances one open cell, shots raycast against the state at that
point, and revives restore dead players while preserving kills.

Clients serialize local input until the prior carrier is indexed and available,
with no queued, submitted, or tentative action. Indexer latency is accepted as
part of play rather than hidden behind a time bucket.

Coordinator ordering is a trust boundary. The coordinator does not run the game
or calculate combat, but it can influence outcomes by delaying, omitting, or
timestamping indexed transactions. A
remote action with no parseable `createdAt` is not replayed and blocks later
actions in that player's authoritative stream. A locally finalized action does
not affect arena state until the indexer supplies `createdAt`. Later historical
indexer data can insert an action earlier in global order and change the replay.
There is no independent consensus beyond the selected Arkade indexer.

## Arena

The arena is one fixed 21x21 layout in every browser. Boundary cells, scattered
barriers, and four recognizable solid houses block movement and shots. The map
is not randomized. The browser renders a smaller camera viewport following the
local player. Selected movement previews its adjacent destination and facing;
selected shots preview the full ray and endpoint; revive has a local indicator.
These previews bridge indexer lag and never mutate authoritative position, HP,
or kills. Snapshots expose `canAct`, optional `projectedAction`, and a bounded
recent shot-trace list.
Each trace is identified by its action txid; camera state remains browser-local.

## Persistence And Recovery

V3 uses `arkade-arena:wallet:v3` and `arkade-arena:pending:v3` browser storage
namespaces plus pending journal version 3. V2 pending state is therefore never
silently restored. Signed Ark and checkpoint PSBTs are persisted before first
submission, and recovery retries the identical transaction and txid. Browser
storage is plaintext and wallet-sensitive.

## Build And Test

Rust 1.86+, Node.js, `wasm-pack`, the `wasm32-unknown-unknown` target, and a
clang with wasm32 support are required.

```sh
cargo fmt --check
cargo test --lib
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
node --check web.js
./build.sh
python3 -m http.server 8000
```

The funded local browser E2E remains isolated behind `regtest-e2e`:

```sh
git submodule update --init
./scripts/test-regtest.sh
```

It builds `pkg-regtest/`, funds a browser wallet, reloads after prepared
issuance/action/sweep journals, verifies that the indexed move changes the
authoritative local coordinates, and checks the six v3 balances and recipient
assets. Manual helpers are available through `./scripts/regtest.sh`. The normal
build and Pages workflow continue to package only the Mutinynet build.

## Limits

- Registration relinquishes 330 sats; the 330-sat carrier remains subject to
  Arkade expiry and future operator policy.
- Anyone can create a canonical registration. There is no identity, admission,
  anti-spam, or asset non-transferability mechanism.
- Fresh clients read all registrations and registered player histories.
- Boarding, settlement, renewal, and unilateral exit are outside this client.
