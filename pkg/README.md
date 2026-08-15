# Arkade Duel

Serverless 1v1 stick-figure shooter. The whole game is a static Rust/WASM
page; every move and every shot is an Arkade transaction, and the match is
replayed and verified from the two players' transaction chains. No game
server, no escrow, no wager.

- Movement inputs (`w`/`a`/`s`/`d` keymask) ride in zero-value OP_RETURN
  outputs of chained self-send transactions.
- Shooting burns one unit of the shooter's per-match bullet asset, so ammo
  is enforced by operator-validated asset conservation.
- The share link is just the host's Arkade address (`/#tark1…`); the joiner
  answers with a `START` transaction and the match id is the START txid.

See [DESIGN.md](DESIGN.md) for the protocol, wire format, and trust model.

## Build

Rust 1.86+ with the `wasm32-unknown-unknown` target, `wasm-pack`, and a
clang that can target wasm32 (for `secp256k1-sys`; on Debian/Ubuntu
`apt install clang` works).

```sh
./build.sh
```

This produces `pkg/arkade_duel.js` and `pkg/arkade_duel_bg.wasm`.

## Run

Serve the repo root with any static file server and open `index.html`:

```sh
python3 -m http.server 8000
# → http://localhost:8000
```

The default Arkade Service is the public **mainnet** operator
(`https://arkade.computer`). Use the network toggle in the wallet panel to
switch to signet (`https://mutinynet.arkade.sh`) for free testing; keys and
match state are scoped per network. Invite links are network-specific (the
`ark1`/`tark1` prefix), and the page warns if a link doesn't match the
current network.

## Play

1. Open the page — a browser key is generated and persisted locally
   (localStorage, exportable under "recovery key"; keys are scoped per
   network).
2. Fund your address by sending **offchain** from any Arkade mainnet wallet
   to your `ark1…` address. Minimum 330 sats (the dust output the event
   chain recirculates); 10k–50k sats is comfortable. Max 0.5 BTC per VTXO.
   Boarding from onchain is not implemented in the WASM client.
3. **Host**: click HOST NEW GAME and send the link. **Join**: open the link
   and click ACCEPT & SEND START TX.
4. WASD moves, SPACE fires. One hit wins. The page polls the indexer and
   replays the canonical input log; when both clients' END state hashes
   match, the result is marked verified.

## Repo layout

| Path | Purpose |
| --- | --- |
| `src/lib.rs` | wasm-bindgen API surface |
| `src/keys.rs` | browser keypair (generate/persist/export) |
| `src/arkade.rs` | Arkade REST client (`fetch` on wasm, reqwest natively) |
| `src/txbuild.rs` | offchain tx build/sign/submit/finalize via `ark-core` |
| `src/gamelog.rs` | OP_RETURN message codec + causal event ordering |
| `src/match_.rs` | match state machine (handshake → issuance → play → end) |
| `src/game.rs` | deterministic integer-only shooter simulation |
| `index.html`, `web.js`, `game.css` | the page |
| `examples/smoke.rs` | live operator smoke test (`cargo run --example smoke`) |

The SDK (`ark-core`) is pinned to commit
`adcb5a8c76ebb14145cddcb0bb685973bc2a981c`.

## Tests

```sh
cargo test --lib        # codec, ordering, sim determinism
cargo run --example smoke   # get_info + address derivation vs live Mutinynet
```

## Status / limits

Alpha software on **mainnet** — real bitcoin, yolo territory. The operator
(`arkd`) is itself alpha; this client has **no settlement, no VTXO renewal,
and no unilateral exit**, so funds left in the browser wallet are not
recoverable if the operator disappears, and VTXOs expire (~7 day unilateral
exit delay). Sweep the balance back to a full Arkade wallet (offchain send)
when you're done playing, and never keep more than you're happy to lose.
Keys live in browser localStorage. The operator validates transactions and
asset conservation, not game rules — game validity is established by
deterministic replay of the public log.
