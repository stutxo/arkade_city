# Design: Registry And Burn Chains

Status: implemented alpha. Native tests, release WASM, and live Mutinynet
smoke tests pass. The public build is pinned to Mutinynet.

## Goal

The game reconstructs one shared maze from ordinary Arkade indexer data. There
are no matches, invite links, hosts, relays, WebSockets, direct browser links,
or changes to the Arkade operator.

Every client performs only these network operations:

- submit its own Arkade transactions;
- read the selected operator's deterministic registry script;
- read scripts learned from valid registrations; and
- fetch creating virtual transactions by txid.

The operator remains an unmodified transaction validator and indexer.

## Registry Address

The application uses `ark_core::UNSPENDABLE_KEY` as the owner of a standard
default VTXO under the selected operator's signer, exit delay, and network.
The public Mutinynet registry is:

```text
owner:   0250929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0
address: tark1qqcpq7yq3e8hhsx6ml3fud93m7827qggaurtzu3zwsr4a0qs0gf85xacghv2fqfrv43e4mgekvqq6ul7u65wm8u7tk3t67kl9syxt822nw9wzn
script:  51201bb845d8a4812365639aed19b3000d73fee6a8ed9f9e5da2bd7adf2c08659d4a
```

The collaborative leaf requires the Arkade server plus NUMS owner. The
unilateral leaf requires the NUMS owner after CSV. Under the unknown discrete
log assumption, neither virtual path is spendable. The operator can still
sweep backing sats after VTXO expiry.

This is a registry sink, not the move ledger. Each player relinquishes one
330-sat output so new clients have a durable, queryable discovery point.
`examples/burnaddr.rs` reconstructs the address from `/v1/info`.

## Player Lifecycle

`App::init` accepts a restored key and pending journal. `web.js` keeps one key
per canonical server URL in `localStorage`; a missing key is generated and
verified in storage before any transaction can be submitted. The raw key,
operator contract parameters, and pending journal are available in the
recovery bundle.

Once the wallet has a valid BTC-only input set, it creates one combined
registration and issuance transaction:

```text
input(s): BTC-only VTXOs totaling at least two times operator dust
output 0: operator dust -> registry script
output 1: operator dust -> player script, carrying all move assets
output 2: optional operator-valid BTC change -> player script
OP_RETURN: Arkade asset issuance packet
anchor: final output
```

The packet creates four immutable groups on output one:

The legacy `arkade-maze-v2` protocol identifier remains immutable so Arkade
City continues to discover and replay transactions created before the rename.

```text
group 0: game=arkade-maze-v2, move=w, amount=50
group 1: game=arkade-maze-v2, move=d, amount=50
group 2: game=arkade-maze-v2, move=s, amount=50
group 3: game=arkade-maze-v2, move=a, amount=50
```

Keeping the asset carrier at exactly 330 sats is an invariant. Extra funding
is separated into BTC-only change. Coin selection requires that change to be
zero or at least `vtxoMinAmount`; the SDK uses the sub-dust form when the
operator permits a value below dust. The issuance txid is the player ID.
Refreshing restores the same wallet, issuance, assets, registration, and dot.

## Move Transaction

For direction `n`, the browser selects the exact-dust carrier containing group
`n` and uses `ark_core::send::build_asset_burn_transactions`:

```text
input:       current 330-sat player carrier
output 0:    330 sats + every unburned asset -> player script
OP_RETURN 1: Arkade Asset V1 extension packet
OP_RETURN 2: 41 4d | 02 | sequence(u32 little endian)
anchor:      final output
```

The selected direction group's output amount is one less than its input
amount. Every other carried group is conserved. The carrier returns unchanged,
so one 330-sat balance can execute all 200 moves. The current Mutinynet service
permits the required two OP_RETURN outputs.

## Registration Validation

For every VTXO on the registry script, a client fetches its creating virtual
transaction and requires:

1. exactly one registry-script output at index zero with the operator dust
   amount;
2. no asset assigned to the registry output;
3. one P2TR player carrier at index one with the same dust amount;
4. exactly four new, non-reissuable asset groups;
5. exactly 50 units per group assigned to output one; and
6. exact game and direction metadata for each canonical group index.

The validated output-one script is stored with the issuance txid. Invalid or
unrelated outputs sent to the public registry are ignored.

## Burn Discovery And Validation

The existing GetVtxos endpoint accepts a protobuf repeated script field through
repeated `scripts` query parameters. Clients deduplicate registered scripts and
query them in batches of 20. Each returned outpoint identifies a creating
virtual transaction.

A move is accepted only when:

1. there is exactly one valid AM v2 receipt;
2. every packet group references an existing asset from one registered
   issuance txid;
3. every packet group has inputs and no reissuance metadata or control asset;
4. exactly one group has `sum(inputs) - sum(outputs) == 1`;
5. all other groups have zero deficit;
6. every preserved assignment targets output zero;
7. output zero is the only positive-value output on the registered player
   script and contains exactly 330 sats; and
8. the indexer reports that output on the expected player script.

This proves a protocol-level burn rather than an application sink transfer.
Current asset-supply summaries are not used to decide historical validity.

## Deterministic Replay

Players do not collide or modify maze cells. A player's position is a pure
function of that player's move stream.

Events are grouped by issuance txid and sorted by `(sequence, move txid)`. The
lower txid wins a duplicate sequence. Replay begins at sequence zero and stops
at the first gap. Late indexer results therefore produce the same full replay
without a global transaction order.

The maze is a 21 by 21 grid with four alternating vertical barriers. A blocked
move still consumes its asset. Reaching `(19, 10)` increments the player's lap
count and resets its dot to `(1, 10)`. There is no terminal game state.

## Pagination

The indexer's VTXO endpoint is paginated and page boundaries can shift while
new records arrive. The first registry and player scans follow every page and
deduplicate outpoints. Later scans stop when a page contains only known or
currently unresolved records. Every 250 cycles a full scan catches records
hidden by unstable boundaries.

Transaction fetch failures remain unresolved and are retried. Discovering a
new registration forces a complete player-script scan, including burns that
predate the observing browser. The local wallet cannot burn another asset
until its own script has completed a full scan with no unresolved records.

## Submission Recovery

Every game or sweep transaction is signed locally and stored as a versioned
browser journal before the first operator request. The next serialized tick
signs a `get-pending-tx` ownership intent for the original inputs and queries
`/v1/tx/pending`. It verifies and finalizes a matching response, or submits the
exact same signed transaction when no pending copy exists. A lost submit or
finalize response leaves the journal intact. It never rebuilds a move or
allocates another sequence.

Every submit response is treated as untrusted. Unsigned Ark and checkpoint
transactions must exactly match the locally built versions. Operator Schnorr
signatures are verified against the original prevouts and scripts. Only those
signatures are merged into local checkpoint PSBTs before the player signs.

The journal stores base64 PSBTs, txid, action kind, sequence metadata, server,
signer, and wallet address. Restore rejects mismatched or malformed journals.
After indexer observation or successful finalization, `web.js` removes and
verifies removal of the journal. If browser storage fails, submission halts.

## Security And Trust

- Asset conservation plus a one-unit packet deficit proves a native burn.
- The issuance txid provides player identity without trusting address metadata.
- Registration binds that identity to one player script used for discovery.
- Operator response validation prevents a returned PSBT from redirecting a
  checkpoint transaction before the player adds its signature.
- The Arkade operator can censor, delay, or equivocate about preconfirmed
  transactions. There is no independent game consensus beyond its indexer.
- Anyone can create a conforming issuance and registration. Participation is
  open and there is no authentication or anti-spam system.
- Asset ownership is transferable. A holder of another player's move asset can
  burn it and attribute a move to that issuance identity.
- Registration backing sats are relinquished. Move-carrier sats remain in the
  player's VTXO chain but are still subject to normal Arkade expiry.
- Game history and player movement are public by design.
- Wallet secrets and pending PSBTs live in plaintext same-origin browser
  storage. An origin compromise is a wallet compromise.

## Deferred Work

- A bounded epoch or checkpoint scheme would cap historical replay cost.
- Registration admission rules could limit script fan-out and spam.
- Boarding, settlement, renewal, and unilateral exit remain outside this WASM
  client.
