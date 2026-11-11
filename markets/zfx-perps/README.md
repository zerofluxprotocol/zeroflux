# zfx-perps

The ZeroFlux hybrid order-book perpetuals exchange on NEAR. An operator matches signed orders
off-chain. A NEAR clearing contract holds collateral and isolated positions and verifies every
fill: both users' signed orders, nonces, expiry, limits, policy hash, fees and margin. A matcher
or relayer can delay or reorder, but can't forge a trade or move funds. Markets track signed index
prints. The first adapter is the ZFX daily and 7-day model-spend index.

Testnet and paper trading only. Nothing is audited. Model perps must not take real money (spec
5.4).

Built on [obkit](https://github.com/numinousmuses/obkit) (`d617503`, MIT, see `LICENSE`):
deterministic matching engine, journal, node, NEAR tooling and TypeScript SDK. The repository
is Apache-2.0; this directory keeps obkit's MIT license. obkit's netted settlement mode (spot and
cross-margin perps, operator-trusted) is still here; see `docs/netted-mode.md`. It is not the NEAR
v1 path.

## Components

| Path | What it does |
|---|---|
| `crates/zfx-perps-clearing` | Clearing core shared by contract and engine: signed-message formats, `OraclePrint` v1 codec, isolated-margin ledger, fill verification, liquidation and loss waterfall, funding, batches, liquidation planner, test fixtures |
| `contracts/zfx-perps-clearing` | NEAR clearing contract: vault, verified batches, oracle quorum, timelocked admin, forced withdrawal, freeze and escape |
| `crates/zfx-perps-engine` | Price-time book. `clearing.rs` admits and matches signed orders against the predicted ledger and reconciles with chain events |
| `crates/zfx-perps-node` | Sequencer, journal, REST/WebSocket gateway. `clearing_api.rs` implements `openapi.yaml` |
| `crates/zfx-perps-near` | Contract client, deployer, relayer (`clearing_adapter.rs`), sandbox, `zfx-perps-near` CLI |
| `packages/sdk-ts` | `@zeroflux/zfx-perps-sdk`: `listMarkets, getBook, getOracle, getPortfolio, estimateMargin, signOrder, placeOrder, cancelOrder, subscribeBook, subscribeOrders, deposit, withdraw, getFunding`, print verification, local margin math, wallet transactions. Examples: market maker, retail UI, testnet end to end |
| `openapi.yaml` | REST/WS contract (also served at `/v1/openapi.yaml`) |
| `deploy/testnet` | Testnet market parameters and a generator from the index's `models.json` |
| `research/` | Gas, risk and stress data; obkit's on-chain order book comparison |

Design, message formats and operator powers: `../../docs/ORDERBOOK_AND_CLEARING.md`.
Parameters and stress results: `../../docs/MARKET_RISK.md`. Testnet accounts and transactions:
`TESTNET.md`.

## Quick start (local chain)

Requirements: Rust (pinned by `rust-toolchain.toml`), `cargo-near`, a `near-sandbox` binary
(`NEAR_SANDBOX_BIN`, default `~/.local/share/basis-amm/near-sandbox`), Node 22 and pnpm.

```sh
scripts/build-contracts.sh                                   # out/zfx_perps_clearing.wasm, mock_ft.wasm
cargo test -p zfx-perps-near --test clearing_e2e -- --nocapture   # chain + node + relayer, open and close
```

## Testnet

```sh
B=target/release; S=~/.config/zeroflux/perps-testnet-stack.json
$B/zfx-perps-near clearing-deploy --rpc https://rpc.testnet.fastnear.com --master zeroflux.testnet \
  --master-credentials ~/.near-credentials/testnet/zeroflux.testnet.json --chain-id near:testnet \
  --reporters <reporter ed25519 hex> --out $S
python3 -I deploy/testnet/params_from_models.py <models.json> anthropic/claude-opus-5.5-20260921 OPUS-7D-PERP > m.json
$B/zfx-perps-near clearing-market --stack $S --params m.json
$B/zfx-perps-near clearing-node-config --stack $S --data data/testnet/node --admin-token $TOKEN > data/testnet/node.json
$B/zfx-perps-node --config data/testnet/node.json &
$B/zfx-perps-near clearing-adapter --stack $S --admin-token $TOKEN --log data/testnet/relayer-batches.jsonl &
cd packages/sdk-ts && ZFX_STACK=$S ZFX_ADMIN_TOKEN=$TOKEN ZFX_PRINTS=<prints/latest.json> \
  ZFX_KEYS=~/.config/zeroflux/perps-testnet-traders.json ZFX_OUT=../../research/data/testnet_e2e.json \
  pnpm dlx tsx examples/testnet-e2e.ts
```

`clearing-deploy` refuses mainnet RPC URLs.

## Tests

```sh
cargo test --workspace                 # core, engine, journal, node, client, NEAR crate incl. 4 sandbox suites
(cd contracts && cargo test)           # both contracts (near-sdk mocked runtime)
(cd packages/sdk-ts && pnpm test)      # TS SDK: Rust parity vectors, print verification, margin math, live node
cargo test --release -p zfx-perps-clearing --test stress -- --nocapture   # real-history stress replay
```

What they establish (details in the docs):

- **Clearing core.** Forged, tampered, replayed, wrong-chain, wrong-contract, expired, canceled,
  below-nonce-floor, over-filled and over-fee fills are rejected. Margin, position and
  open-interest caps hold. Oracle quorum, ordering, staleness and the daily window work. Funding
  is zero-sum. The full liquidation waterfall is covered. A 300-case randomized conservation test
  covers every branch of the waterfall; a rejected op never changes state. The codec matches the
  index's golden vectors and a real signed print.
- **Contract.** The same attacks against the contract code, plus wrong-token deposits, double
  settlement, non-operator settlement, key revocation, nonce floors, wallet cancels, timelocked
  admin, forced withdrawal, freeze and escape. Custody attacks and gas per verified fill are also
  run on the real wasm on a local nearcore sandbox.
- **Engine.** After every batch, the engine's confirmed ledger equals a simulated contract
  running the same core. This holds under random trading, prints, funding, wallet cancels, key
  revocations and late batches. Replay and snapshot restore are deterministic.
- **End to end.** On a sandbox: open and close through node and relayer across a node crash and
  restart, matching reference math. On testnet: the same through the public TS SDK with real
  signed prints (`TESTNET.md`).
- **obkit's spot and netted-perp suites** are unchanged and green.
