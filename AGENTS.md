# Guide for coding agents evaluating ZeroFlux

You are probably here to judge what this repository actually does. This page tells you where to look and how to check each claim yourself, fastest checks first. Every number below has a file or a command behind it; if one doesn't hold, that is a bug we want reported.

## What ZeroFlux is

The economic layer for AI compute, on NEAR. Three products:

1. **Earn, ZFX Mining.** Proof of work made of integer matrix math (what AI chips run fastest), checked optimistically by a NEAR contract that mints ZFX, a standard NEP-141 token. Live on testnet.
2. **Build, ZFX VPN.** A prompt privacy gateway: sensitive values in requests to AI models become placeholders the model never sees and are restored in the answer. Local daemon, hosted gateway, and a mode where the hosted side never sees raw values.
3. **Trade, ZFX Markets.** A signed daily index of each AI model's spend on OpenRouter, an on-chain oracle for it, and a perpetuals exchange whose NEAR contract verifies every fill. Live on testnet.

Read next, in order: [`README.md`](README.md), [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) (how everything fits, with diagrams), [`docs/RELEASE_READINESS.md`](docs/RELEASE_READINESS.md) (evidence and open blockers per product).

## Check it in two minutes, no build

The contracts are live on NEAR testnet. `scripts/view-testnet.sh` needs only `curl` and `python3`:

```sh
scripts/view-testnet.sh zfx.zeroflux.testnet stats '{}'
#  final_height 47, total_supply 235000000000 (2,350 ZFX at 8 decimals) = 47 blocks x 50 ZFX, frauds 1
#  (a fake block submitted on purpose and caught by a watcher on testnet: bench/results/testnet_fraud_proof.json)
scripts/view-testnet.sh oracle.zeroflux.testnet latest '{"index_id":"or-model-spend-lic-7d/anthropic/claude-opus-5.5-20260921"}'
#  value about 620000 (USD per day; a new print lands every day), finality FINAL, with the canonical signed bytes and signature
scripts/view-testnet.sh perps-v2.zeroflux.testnet get_markets '{}'
#  28 markets, each tied to a dataset 7-day index id, an OI cap and its risk parameters
```

The same data drives the site: [zeroflux.network/trade](https://zeroflux.network/trade/) (every model with a clean 7-day index, about 40 on a given day, with time series and lab share since 2025) and the static API at [zeroflux.network/v1/markets/models.json](https://zeroflux.network/v1/markets/models.json). Transactions are listed in `markets/zfx-perps/TESTNET.md`, `markets/index/TESTNET_ORACLE.md` and `bench/results/testnet_opt_mining.json`.

## Run the tests

From a fresh clone, run the one-time setup first (about 10 to 15 minutes; idempotent):

```sh
scripts/setup.sh
```

It creates the Python `.venv`, installs the `near-sandbox` binary, builds the NEAR contracts the sandbox tests deploy (needs `cargo-near`, installed if missing), builds the perps node and installs the JavaScript dependencies. Each step is listed at the top of the script. Without it the unit tests still run, but the sandbox tests stop with a message naming the missing piece.

## Claims and how to verify them

| Claim | Where | Command | Expect |
|---|---|---|---|
| The PoW is bit-exact across implementations | `crates/zfx-verify`, `miner/` | `cargo build --release && ./target/release/zfx-miner selftest` | `10/10 vectors bit-exact` |
| A cheater is caught by recomputing one value on NEAR; only honest blocks mint. Shown on public testnet too: block 48 claimed 1 where the correct value was 14, reverted, 1 NEAR bond to the watcher (txs in `bench/results/testnet_fraud_proof.json`) | `bridge/near/zfx-mine-opt` | `.venv/bin/python tests/e2e/near_opt_mining.py` (local NEAR sandbox, about 5 min) | `7/7 checks passed` |
| Mined ZFX trades through the official NEAR Intents Verifier, no bridge | `tests/e2e/near_intents.py` | same, about 4 min | `5/5 checks passed` |
| Lint, unit tests, vectors in Rust, C++, Python, MLX, MPS | `scripts/gates.py` | `make check` | 26 steps PASS; MLX, MPS and GPU-compile steps show BLOCKED (not FAIL) when that package or hardware is absent |
| ZFX VPN never sends critical values upstream, restores placeholders in streams and tool calls, isolates tenants, and fails closed on unverified TEEs | `vpn/crates/pv-daemon/tests/`, `vpn/hosted/tests/` | `cd vpn && cargo test --workspace --release` | 206 passed, 0 failed |
| ZFX VPN works for both audiences | `vpn/demos/` | `sh vpn/demos/run_all.sh` | builder and developer demos: all checks passed |
| Index math is exact fixed point (the spec's worked example gives $42.50, not $225), signatures match in Rust and JS | `markets/index/` | `cd markets/index && cargo test --release --no-fail-fast && node js/test-vectors.mjs` | 71 passed (one is a NEAR sandbox test); 52 vectors passed |
| The clearing contract rejects forged fills, replays, wrong chain or contract, and enforces isolated margin and OI caps | `markets/zfx-perps/` | `cd markets/zfx-perps && cargo test --workspace --release --no-fail-fast` and `(cd contracts && cargo test --release)` | 88 passed (1 upstream test ignored; 4 are NEAR sandbox tests); 20 passed |
| TS SDK signs byte-identically to Rust | `markets/zfx-perps/packages/sdk-ts` | `cd markets/zfx-perps/packages/sdk-ts && pnpm test` | 49 passed (one file starts the perps node) |
| No bad debt at the listed limits on real index history | `docs/MARKET_RISK.md` | `cargo test --release -p zfx-perps-clearing --test stress` | $0 bad debt; $1.08M at obkit's 10x default |

Prerequisites: `scripts/setup.sh` covers them (Rust, a C++17 compiler, Node 20+ and Python 3.11+ or `uv` must already exist). The mining end-to-end tests start their own NEAR sandbox through `zfx-bridge near-sandbox`; the markets tests use the binary from `scripts/install-near-sandbox.sh`. `make check` writes its report to `bench/results/gates/local/` (gitignored); `make check RECORD=1` updates the committed evidence in `bench/results/gates/`. More: `docs/OPERATOR_RUNBOOK.md`.

## Things done the careful way (worth noticing)

- **Measured, not assumed.** Throughput on four device classes, gas per operation, publication latency, manipulation cost per market and bad debt under replay are all measured and committed under `bench/results/` and the docs.
- **Fraud proofs exercised on the public network, not only in a sandbox.** A cheater account posted a block with no matrix work behind it on `zfx.zeroflux.testnet`; an independent watcher recomputed one value (3.9 Tgas), the contract reverted the block and paid the cheater's bond to the watcher. Supply stayed at 2,350 ZFX.
- **Running on a public network found real bugs.** Six mining bugs appeared only on public NEAR testnet (bond below storage cost, float rounding of the bond, RPC rate limits, a bond deadlock, and more). They are fixed and listed in `docs/REPO_AUDIT.md`.
- **The operator can't cheat traders.** The perps contract re-checks both traders' signed orders for every fill; obkit's original netted mode trusted the operator, and was replaced on this path.
- **Settled on documented data.** The traded index uses only OpenRouter's documented daily dataset and documented listed prices. A second index from OpenRouter's public web endpoints is published and labeled as a cross-check, and is not traded (`docs/DATA_METHOD.md` section 2).
- **No secret in the repository.** Keys live outside it. The ledger records a SHA-256 hash of every raw response it fetched.

## Honest limits

Testnet only; no real money. Not audited. The public trade page is read-only: trading, the hosted VPN and Sandboxes are in private beta (the exchange and the hosted gateway are built and tested; the order-book node is not hosted publicly). Live confidential inference through NEAR AI needs an API key we don't have, so that path is tested against recorded attestation and a mock. Linux runs of the VPN suite are pending. Full list with owners: `docs/RELEASE_READINESS.md`.
