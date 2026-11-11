# Release readiness

Three products, each with its own readiness. Engineering facts are separated from owner decisions. Nothing here issues, distributes or markets a live token; nothing is deployed to NEAR mainnet; nothing handles real money.

```
MINING_TESTNET_READY      = true   (zfx.zeroflux.testnet, anyone can mine with scripts/mine-testnet.sh)
MINING_MAINNET_READY      = false
VPN_LOCAL_READY           = macOS: public beta of local mode; Linux: not run
VPN_HOSTED                = built and tested; private beta (zeroflux.network/#beta), not served publicly
VPN_PRODUCTION_READY      = false
INDEX_PUBLISHED           = true   (daily, signed; paper trading only)
INDEX_SETTLEMENT_READY    = false
PERPS_TESTNET_READY       = true for operator-run testing (perps-v2.zeroflux.testnet); public trading in private beta
PERPS_REAL_MONEY_READY    = false
```

## Earn: ZFX Mining

ZFX is minted by the `zfx-mine-opt` contract for valid proof of work, checked optimistically (`docs/NEAR_SETTLEMENT.md`). The evidence matrix is `ACCEPTANCE_REPORT.md`.

### Verified (with evidence)

| feature | evidence |
|---|---|
| ZFX mined on public NEAR testnet at `zfx.zeroflux.testnet` with `scripts/mine-testnet.sh` on an Apple GPU: 29 blocks in 15 minutes; 47 blocks finalized in total, 2,350 ZFX minted, supply equal to reward x finalized height | `bench/results/testnet_opt_mining.json` |
| ZFX mined on NEAR with the production proof of work, checked optimistically: honest blocks never challenged; cheaters caught at every layer with one-value fraud proofs; descendants of a fraudulent block reverted; only honest blocks mint | `tests/e2e/near_opt_mining.py` (7/7, including O7: a bond below a pending block's storage cost is refused) |
| A fraud proof on public testnet: `cheater-demo.zeroflux.testnet` posted block 48 with no matrix work (tx `CJxJJY7ybZyeDiEwhteGddPt3wpS49VVB4vhKjcXG6LH`); `watcher-demo.zeroflux.testnet` challenged layer 0 row 0, claimed 1 vs correct 14, 3.9 Tgas (tx `AnAydzbXabjBT2Vd7MjYvYWrAC2zgSV3vjUy6W3Cshhb`); the block was reverted, the 1 NEAR bond went to the watcher, supply unchanged | `bench/results/testnet_fraud_proof.json` |
| ZFX trades through the official NEAR Intents Verifier with no bridge | `tests/e2e/near_intents.py` (5/5) |
| Alternatives on NEAR: every block checked in full (`zfx-mine`), PoW light client (`zfx-light`) | `tests/e2e/near_mining.py` (8/8), `near_light_client.py` (6/6) |
| ZFX-FP8C PoW, bit-exact across Rust, C++, Python on CPU, Apple GPU (MLX, MPS), NVIDIA H100, AMD MI355X | `make check`, `bench/results/gpu_summary.json`, `bench/results/gpu_rerun_*` |
| Real blocks mined by GPU miners on the production v0.1 profile, validated by a CPU node and re-validated by an independent CPU node | `bench/results/gpu_rerun_*/*/results/results.jsonl` (`gpu_block_submissions`) |
| Consensus v2: state root, coinbase maturity, memos, native bridge module, RunningMax LWMA | `docs/CONSENSUS.md`, `tests/vectors/consensus_v2.json` |
| Own-chain alternative: 3-node network, 1000+ blocks, encrypted transport, restarts, corruption repair, partition and reorg | `make e2e-native` |
| Own-chain alternative, committee bridge: 100 two-way transfers under fault injection, exact reconciliation, finality and reorg handling, governance and recovery | `make e2e-bridge` |
| NEAR Intents: official Verifier, quotes, settlement both ways, expiry, liquidity limits, duplicate and replay rejection, refunds | `make e2e-intents` |
| Parser fuzzing, sanitizers, dependency audit, SBOM, license gate, reproducible build | `make security-test`, `bench/results/release_check.json` |
| Versioned RPC schema checked against a live node | `schemas/`, `bench/results/contract_api.json` |


### Blocking issues for MINING_MAINNET_READY

| # | blocker | status | owner |
|---|---|---|---|
| 1 | External cryptanalysis of the S-box-chain hardness assumption | not started | external reviewers |
| 2 | Written soundness argument for the PoW | not started | ZeroFlux research |
| 3 | External security audit of `zfx-mine-opt`, the watcher and the mining proxy | not started | external auditor |
| 4 | Watcher coverage on a public network: independent watchers, alerting, challenge-window and bond sizing | watcher ships with the mining tools; parameters are testnet values | ZeroFlux core |
| 5 | Monetary schedule (reward curve, tail, supply) | the contract pays a constant testnet reward; schedules are modeled (`bench/results/tokenomics.json`, recommendation `D_smooth_decay_tail`) | project owner (decision) |
| 6 | Exactness on B200, RTX 50 and MI300X hardware | B300, H200, RTX PRO 6000 and RTX 4090 measured exact on every path on 2026-11-11 (`docs/HARDWARE.md`); B200 and RTX 5090 had no cloud capacity that day | ZeroFlux core (needs hardware access) |
| 7 | NEAR testnet deployment of `zfx-mine-opt` | done: `zfx.zeroflux.testnet` (deploy tx `J7RKZCGGtrqTpGGhzezR64gAo9gdCzmeopNsWcCw5Acf`); `mine.zeroflux.testnet` is retired (bond below storage cost) | done |
| 8 | Live Intents access and liquidity | ZFX works with the official Verifier; the public Message Bus needs a Partner JWT; no liquidity solicited | project owner (partner program) |
| 9 | Release signing key and signed artifacts | no key exists | project owner |
| 10 | Economic, operational and legal review | not started | project owner |


## Build: ZFX VPN

```
VPN_LOCAL_READY      = macOS: yes for a public beta of local mode; Linux: not run
VPN_HOSTED_READY     = false (service code and tests done; no deployment, KMS, shared state, review)
VPN_PRODUCTION_READY = false
CONFIDENTIAL_CLAIMS  = attestation verified live; confidential inference itself not exercised (no NEAR key)
```

### Verified (with evidence)

| Gate (brief 3.7) | Evidence |
|---|---|
| 1. Existing local policies, keys and audit DB survive upgrade and rollback; old CLI alias works | `cargo test -p pv-daemon --test migration` (5 tests, fixture written by the Prompt VPN 0.1 binary: byte-identical ciphertexts, key file, MAC chain, policy, app keys; keychain copy and rotated-key rollback with an in-memory store). `sh tests/migration/upgrade_rollback.sh` (20 checks: builds 0.1 from the import commit, upgrades through the `promptvpn` link, uses the install, rolls back, 0.1 serves it again) |
| 2. Synthetic critical PII and API keys never reach a public upstream; exact egress bytes | `streaming_redteam.rs::exact_egress_bytes_never_contain_critical_values` (6 values x 6 field placements x stream/non-stream, plus base64 forms; asserts recorded bodies and every header), `gates.rs`, `adversarial.rs` |
| 3. Faithful restoration in non-stream, SSE chunks and tool-call args; conversation and tenant isolation | `streaming_redteam.rs` (SSE split 1 to 64 bytes per write, UTF-8 and JSON split, streamed tool args, 24 concurrent conversations over 2 apps), `hosted.rs::maps_are_per_request_and_tenants_are_isolated` |
| 4. TEE-required route rejects unverified proxy, expired measurement, unavailable E2EE, route substitution, credential crossover | `tee_gates.rs` (8 tests, mutation-checked), `tee.rs` unit tests, `near_replay.rs` (real NEAR report recorded 2026-11-10, offline DCAP; tampering rejected), `live_near.rs --ignored` (passed live 2026-11-10) |
| 5. Hosted raw-prompt disclosure; local-to-hosted transmits no known raw secret | `hosted.rs::disclosure_is_explicit`, `protected_transport.rs` (relay records exact bytes reaching the hosted side; hosted refuses to transform preprocessed traffic) |
| 6. Chat + Responses, stream + non-stream; cancellation, timeouts, malformed chunks, retries, filtering; no downgrade | `streaming_redteam.rs`, `routing.rs`, `tee_gates.rs`, `gates.rs`, `hosted.rs` |
| 7. Onboarding, notifications, approvals, classifier install, NEAR skip, dashboard, packages on real macOS/Linux | Partly: dashboard rebuilt and type-checked, approvals and onboarding tests pass (`approvals.rs`, `gates.rs`), skill script tests pass. Not re-run for this release: the clean-install gate from a release archive, headless dashboard screenshot check, Linux |
| Both audiences demoed | `sh demos/run_all.sh` (builder app through hosted gateway; developer coding-agent session through local daemon, then protected transport) |

### Blockers for VPN_PRODUCTION_READY

| # | Blocker | Status | Owner |
|---|---|---|---|
| 1 | NEAR AI API key to exercise live confidential inference, E2EE (v3) and `provider_tee` response signatures end to end | Code path uses NEAR's official SDK; tested only with a mock backend | Project owner (key) |
| 2 | Third-party security review of the local daemon, hosted gateway and migration | Not started | External reviewer |
| 3 | Hosted deployment design: KMS for tenant vault keys (today a key file per tenant), shared idempotency and rate-limit store for more than one instance, no core dumps, restricted operator access, audited images | Single-process service only | ZeroFlux core |
| 4 | Hosted terms of service and privacy policy text that matches the disclosure (operator sees raw prompts in hosted mode) | Disclosure exists in API and docs; no legal text | Project owner, counsel |
| 5 | Linux run of the full check (`sh scripts/check.sh` on Linux x86_64 and arm64), including Secret Service keyring and notifications | Never run; no Linux host | ZeroFlux core |
| 6 | Real macOS Keychain upgrade run by a person (one "Always Allow" prompt) | Unattended run blocked by the OS prompt | Project owner (5 minutes at the Mac) |
| 7 | Measurement pin policy for NEAR models (which compose / OS image hashes to accept, for how long) | Mechanism built; no pins shipped; NEAR publishes no allowlist | Project owner + security reviewer |
| 8 | GLiNER capacity plan for the hosted service (one model instance serializes requests; see hosting numbers) | Measured on this Mac only | ZeroFlux core |
| 9 | Re-render demo videos with the new name | Done 2026-11-11: Launch, VPN and Markets videos use ZFX VPN | Design |


### Owner decisions

- Public hosted endpoint name (`api.zeroflux.network` per the brief) and who operates it.
- Whether hosted idempotent replay may keep results in memory (default: no).
- Default for confidential fallback in onboarding (today: off; local review is the default for uncertain content).
- Whether to accept NEAR models whose TCB is `OutOfDate` (today: rejected for confidential routes).


Full threat model: `docs/VPN_THREAT_MODEL.md`. Measured hosting resources: `vpn/docs/hosting-resources.md`.

## Trade: ZFX Markets index

```
INDEX_PUBLISHED        = true (daily; 54 markets; web and dataset indices, daily and 7-day)
INDEX_SETTLEMENT_READY = false
TRADED_INDEX           = dataset 7-day (OpenRouter's documented datasets/rankings-daily, plus documented listed prices)
```

### Verified (with evidence)

| feature | evidence |
|---|---|
| Fixed-point formula, UTC day boundaries, the spec's worked example ($42.50, not $225), NOT_REPORTED and UNPRICED states, corrections as new revisions, integrity checks | `cd markets/index && cargo test` (63 passed) |
| OraclePrint v1: frozen canonical bytes, ed25519 signatures, identical in Rust (also wasm32) and JavaScript | `vectors/oracle_print_v1.json`, `node js/test-vectors.mjs` (52 passed) |
| Append-only ledger: hash chains, raw response hashes, every print verifies | `zfx-index verify`: 6,086 prints valid, 0 invalid |
| Documented dataset backfill 2025-01-01 to 2026-11-09 (678 days, 445 models); documented and web token totals identical on all 1,174 overlapping market-days | `docs/DATA_METHOD.md` section 13 |
| 7-day index with manipulation-cost metrics and OI caps (25% of the 7-day spend sum); jump clamp 25% (p99 of 22,654 observed 7-day moves) | `docs/DATA_METHOD.md` sections 12 and 13 |

### Blocking issues for INDEX_SETTLEMENT_READY

| # | blocker | status | owner |
|---|---|---|---|
| 1 | Fields no public endpoint provides: billed-dollar totals, a corrections and finality policy, intraday data, coverage beyond the daily top 50 (`docs/DATA_METHOD.md` section 10) | not available | project owner |
| 2 | Publication latency over 30 days (spec 4.3) | 1 day measured: the dataset had the new day 26 minutes after midnight UTC | ZeroFlux core (time) |
| 3 | First-party wash trading: a provider serving its own model gets most of a push back, so its real manipulation cost is below the published figure; price cuts move the index down for free | documented; not priced into caps | project owner + market risk review |
| 4 | Listing rule for trading: a market needs a sustained top-50 record, because the dataset index needs top-50 membership on every day of its window | Models outside the top 50 stay tracked through the web index (published and on chain); the trade app shows a model when its dataset 7-day index is OK, and offers trading when it is also a listed perps market with an OI cap of at least $10,000 (27 today) | project owner |
| 5 | The index poller runs on one Mac and stops when it sleeps | runs from `~/.local/share/zfx-index` | project owner (hosting decision) |

## Trade: ZFX Markets perps

```
PERPS_TESTNET_READY    = true for operator-run testing (perps-v2.zeroflux.testnet, 28 markets); no public venue node yet
PERPS_REAL_MONEY_READY = false
```

### Verified (with evidence)

| feature | evidence |
|---|---|
| The NEAR clearing contract verifies every fill: both signed orders, registered keys, chain and contract id, nonces, expiry, policy hash, limits and price band, remaining quantity, fee caps, isolated margin, position and open-interest caps. Forged fills, replays, wrong chain or contract, double settlement and fee misaccounting are refused | `cd markets/zfx-perps && cargo test --workspace --release --no-fail-fast` (88 passed, 0 failed, 1 upstream live-ticker test ignored); `contracts && cargo test --release` (20 passed); sandbox attacks against the real wasm |
| Engine and contract agree: after every batch the engine's ledger equals a simulated contract, including on-chain races | 300-case property tests, replay and snapshot determinism |
| Signed index oracle: allowlisted reporters, quorum, forward-only prints, corrections chained; stale, NOT_REPORTED or clamped (25%, derived from consecutive signed prints; matches the publisher's flag on all 882 OK dataset 7-day days) forces close-only | clearing core tests; `markets/zfx-perps/TESTNET.md` |
| Loss waterfall with no protocol backstop; replay of all 50 real index histories: $0 bad debt with the listed parameters (2x max leverage, mark limit), versus $1.08M at obkit's 10x default | `docs/MARKET_RISK.md`, `cargo test --release -p zfx-perps-clearing --test stress` |
| Testnet end to end through the public TS SDK: open and close on the dataset 7-day Opus market, balances equal reference math to the atom | `markets/zfx-perps/TESTNET.md` |
| A fresh faucet account onboarded with `zfx-perps-near onboard-testnet`, then a signed limit order placed and cancelled from the site's trade app in a browser; the cancel settled on-chain | `TESTNET.md`; batch `BAvk2qZpFLBd1kuiVSiGqDbttB6KvycmJfTemrzmibLk` (after the operator key rotation) |
| TS SDK: 13 methods, bigint money, OpenAPI, Rust/TS signature parity | `packages/sdk-ts`: 49 passed |
| On-chain index oracle `oracle.zeroflux.testnet`: 923 signed prints (both 7-day indices for every tracked model), daily pushes; forged signature, outsider key, stale window, replay and unchained correction refused | `markets/index/TESTNET_ORACLE.md`; `cd markets/index && cargo test` (71 passed), contract tests 12 passed |

### Blocking issues for PERPS_REAL_MONEY_READY

| # | blocker | status | owner |
|---|---|---|---|
| 1 | Counsel: venue, product, clearing, jurisdiction, sanctions and AML (spec 5.4; CFTC interest in compute derivatives) | not started | project owner |
| 2 | Independent smart-contract audit of the clearing contract, the oracle contract and the relayer | not started | external auditor |
| 3 | Owner keys: multisig and a timelock of days, not 60 s; the testnet owner is `zeroflux.testnet` | testnet only | project owner |
| 4 | Insurance fund size and auto-deleveraging ranking (no data yet: the stress replay seeds 2% of open notional) | policy open | project owner + risk review |
| 5 | Index publication latency over 30 days; the 22:00 UTC close-only window is a placeholder from one sample | collecting | ZeroFlux core (time) |
| 6 | A public venue node (operator node + relayer) so site visitors can trade on testnet | runs on one Mac today | project owner (hosting decision) |
| 7 | Funding for the oracle's storage: about 0.29 NEAR a day pushing both 7-day indices for every tracked model (about 108 prints a day); the contract holds about 10.6 NEAR above its reserve (about 36 days) | funded 2026-11-11; top up with `zfx-oracle-near transfer` | ZeroFlux core |

### Key rotation, 2026-11-11

A coordinator-side agent printed two testnet secret keys into its local session output while locating credentials (never written to files or the repository). Both were rotated the same day: the `zeroflux.testnet` key (also a full-access key on `zfx`, `mine` and `wzfx.zeroflux.testnet`) and the `perps-v2-operator.zeroflux.testnet` key. The old keys are deleted on-chain; the relayer was restarted with the new operator key and settled batch 11 afterwards.

Legal gate (spec 5.4): no real-money or US-facing deployment before counsel, audits, risk stress tests, oracle reliability and emergency operations sign-off.
