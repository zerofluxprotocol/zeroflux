# Release readiness

```
PRIVATE_TESTNET_READY = true
MAINNET_READY         = false
BRIDGE                = 2-of-3 trusted custodian (testnet only)
```

`PRIVATE_TESTNET_READY` means: a multi-node network with no monetary value runs, mines with three independent miner implementations, transfers with hash-based signatures, survives crashes, corruption and reorgs, and bridges to a NEAR sandbox and through NEAR Intents with exact accounting. The evidence matrix is `ACCEPTANCE_REPORT.md`; each line there names the command and its result.

## Verified (with evidence)

| feature | evidence |
|---|---|
| ZFX-FP8C PoW, bit-exact across Rust, C++, Python on CPU, Apple GPU (MLX, MPS), NVIDIA H100, AMD MI355X | `make check`, `bench/results/gpu_summary.json`, `bench/results/gpu_rerun_*` |
| Real blocks mined by GPU miners on the production v0.1 profile, validated by a CPU node and re-validated by an independent CPU node | `bench/results/gpu_rerun_*/*/results/results.jsonl` (`gpu_block_submissions`) |
| Consensus v2: state root, coinbase maturity, memos, native bridge module, RunningMax LWMA | `docs/CONSENSUS.md`, `tests/vectors/consensus_v2.json` |
| 3-node network, 1000+ blocks, encrypted transport, restarts, corruption repair, partition and reorg | `make e2e-native` |
| Bridge: 100 two-way transfers under fault injection, exact reconciliation, finality and reorg handling, governance and recovery | `make e2e-bridge` |
| NEAR Intents: official Verifier, quotes, settlement both ways, expiry, liquidity limits, duplicate and replay rejection, refunds | `make e2e-intents` |
| Parser fuzzing, sanitizers, dependency audit, SBOM, license gate, reproducible build | `make security-test`, `bench/results/release_check.json` |
| Versioned RPC schema checked against a live node | `schemas/`, `bench/results/contract_api.json` |

## Blocking issues for MAINNET_READY

| # | blocker | status | owner |
|---|---|---|---|
| 1 | External cryptanalysis of the S-box-chain hardness assumption | not started | external reviewers |
| 2 | Written soundness argument for the PoW | not started | ZeroFlux research |
| 3 | External security audit: node, P2P transport, SLH-DSA/P2MR integration, wallet, wZFX contract, relayers | not started | external auditor |
| 4 | Exactness on B200/B300, RTX 50, RTX PRO 6000 and MI300X hardware | code compiles for sm_100/sm_120/gfx942; no device reached | ZeroFlux core (needs hardware access) |
| 5 | Monetary schedule (block time, subsidy curve, tail, supply) | parameterized and modeled; recommendation `D_smooth_decay_tail`; node refuses `network: mainnet` until chosen | project owner (owner decision) |
| 6 | Bridge beyond a trusted custodian | measured infeasible on NEAR today (`onchain_pow_gas.json`, `stark_prototype.json`); committee remains | ZeroFlux research, then external review |
| 7 | Undo-based reorgs instead of replay from genesis; stronger eclipse resistance | open | ZeroFlux core |
| 8 | DoS economics of PoW-valid spam; encrypted wallet files | open | ZeroFlux core |
| 9 | Release signing key and signed artifacts; container images built and scanned | no key exists; Docker not available on the build host | project owner (key), ZeroFlux ops |
| 10 | Public testnet with persistent relayers and explorer | wZFX is deployed at `wzfx.zeroflux.testnet` and a full round trip ran there (`bench/results/testnet_smoke.json`); the ZeroFlux side and relayers ran locally for the test and are not hosted | ZeroFlux ops |
| 11 | Live Intents access and liquidity | adapter ready; public Message Bus needs a Partner JWT; no liquidity solicited | project owner (partner program) |
| 12 | Economic, operational and legal review | not started | project owner |

Nothing in this repository issues, distributes or markets a live token. Nothing is deployed to NEAR mainnet. On NEAR testnet: `zeroflux.testnet` (faucet-funded), `wzfx.zeroflux.testnet` (wZFX contract), `coordinator.zeroflux.testnet`, `user0.zeroflux.testnet`.
