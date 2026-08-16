# Design: Arkade City V3

## Boundary

The static browser app owns transaction construction, validation, and game
replay. The selected Arkade operator remains an unmodified finalizer and
indexer. Network operations are limited to submitting the player's own
transactions, querying the operator-specific NUMS registry and registered
player scripts, and fetching creating virtual transactions.

The public build accepts only the pinned Mutinynet URL and verifies the derived
NUMS registry against `GAME_ADDRESS`. The `regtest-e2e` feature additionally
accepts the pinned local test operator. Both derive the registry from the same
standard Arkade NUMS owner and operator contract parameters.

## Registration

Protocol identity is `arkade-arena-v3`; no v2 metadata is accepted. One
canonical transaction spends BTC-only inputs and creates:

```text
output 0: operator dust (currently 330 sats) -> NUMS registry
output 1: operator dust -> player script, carrying all action assets
output 2: optional operator-valid BTC change -> player script
OP_RETURN: Arkade asset issuance packet
anchor: final output
```

The packet has exactly six immutable groups. Group metadata and supplies are
`w=50`, `d=50`, `s=50`, `a=50`, `bullet=50`, and `life=5`, each assigned only
to output 1. The issuance txid is the player ID. Registration validation also
requires one registry output at index 0, one P2TR player carrier at index 1,
exact dust values, no registry assets, and no unrelated receipt payload.

Because these groups have no control asset, their IDs cannot be reissued. The
temporary Mint New Action Pack flow therefore publishes another complete
registration from 660 sats of BTC-only inputs. Its issuance txid becomes the
active player ID and resets position, HP, kills, and sequence; the prior player
and its remaining immutable assets stay discoverable. Fresh wallet recovery
selects the newest spendable pack by indexed carrier creation time.

## Actions

All six actions use the same native asset-burn transaction shape:

```text
input:       current exact-dust player carrier
output 0:    exact-dust carrier + every unburned unit -> player script
OP_RETURN 1: Arkade Asset V1 extension packet
OP_RETURN 2: 41 4d | 03 | sequence(u32 little endian)
anchor:      final output
```

Exactly one group has `sum(inputs)-sum(outputs)=1`; every other included group
has zero deficit. Preserved assignments must target output 0. The burn is an
action even when game rules later make it a no-op, such as shooting while dead
or reviving while alive.

## Discovery And Replay

Registry and player VTXO pages are fully scanned initially, deduplicated by
outpoint, and periodically rescanned. API page order is never game order.
`createdAt` is deserialized as a string and parsed to `i64` in the REST client.

Events are grouped by player ID into a checkpoint-aware predecessor chain.
Competing candidates for the same chain position use indexed `(createdAt,
txid)` ordering; acceptance starts at sequence zero and stops at a gap. A
missing or unparseable `createdAt` also stops a remote player's authoritative
stream. Every canonical chain event remains available to inventory and
`next_sequence` accounting.

Every canonical event with `createdAt` enters gameplay exactly once. Replay
sorts all players' events globally by `(createdAt, txid)` and applies each one
sequentially. Later historical indexer data can insert an event earlier in that
ordering and change the reconstructed result.

Locally finalized events are allowed a temporary timestamp-less chain marker.
Their carrier output remains unresolved when the indexer omits `createdAt`, and
they never affect authoritative arena state. Rediscovery updates the existing
event timestamp and removes tentative status rather than suppressing it through
txid deduplication. Tentative actions never affect arena state.

This creates an explicit coordinator-order trust caveat: the operator does not
execute arena rules, but its indexed timestamps can affect cross-player combat
outcomes. It may censor, delay, omit, timestamp, or equivocate. There is no
independent game consensus beyond the chosen indexer.

## Deterministic Arena

The map is a fixed 21x21 square. Boundary walls, static barriers, and four
solid house footprints are compile-time deterministic and exposed in snapshots.
Spawn is a deterministic txid hash into the ordered list of walkable interior
cells. Players may overlap and initially have 3 HP with right-facing direction.

Replay rules:

- Every canonical indexed action is applied in global `(createdAt, txid)` order.
- Alive movement sets facing even if blocked, then moves one cell if open.
- An alive shooter raycasts from the position and facing at that point in replay.
- Rays hit the nearest living player; overlap ties use lowest player txid.
- A hit removes 1 HP and awards its shooter a kill on a transition to zero.
- Dead movement and shooting are no-ops.
- Revive restores a dead player to 3 HP at spawn, facing right.
- Kills persist through revive.
- Leaderboard order is kills descending and player txid ascending.

Snapshots include `canAct`, optional `projectedAction`, and a bounded list of
recent shot traces. `projectedAction` is the queued, submitted, or local
timestamp-less action and disappears when indexed replay becomes authoritative.
Each trace contains its action txid as a stable identity,
shooter ID, start/end cells, and optional hit player. Miss endpoints are the
first obstacle or boundary cell. Camera follow and trace animation are local UI
concerns. Movement, firing rays, and revive are previewed locally, but previews
never mutate authoritative arena state and require no rollback path.

## Durable Browser State

Wallet and pending storage are namespaced under `arkade-arena:*:v3`; the Rust
pending journal version is 3 and issuance journals contain six asset IDs. This
prevents v2 pending transactions from being restored into v3. The existing
prepare-persist-submit-finalize sequence and exact-PSBT recovery remain intact.

## Security And Limits

- The coordinator validates transactions and asset conservation, not game rules.
- Anyone can register and assets can be transferred, so issuance identity is
  not a person or a non-transferable credential.
- Registration sats are relinquished and carrier sats remain subject to normal
  Arkade expiry.
- Wallet secrets and pending PSBTs are plaintext same-origin browser data.
- Full historical replay grows with players and actions.
