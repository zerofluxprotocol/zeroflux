# Release readiness

```
ARCHITECTURE  = ZFX is a NEAR token minted by proof of work, checked optimistically on NEAR (bridge/near/zfx-mine-opt)
TESTNET_READY = local sandbox end to end; not deployed to NEAR testnet
MAINNET_READY = false
```

ZeroFlux is a proof-of-work network on NEAR: ZFX is minted by the `zfx-mine-opt` contract for valid proof of work, checked optimistically (`docs/NEAR_SETTLEMENT.md`). The evidence matrix is `ACCEPTANCE_REPORT.md`; each line there names the command and its result.

## Verified (with evidence)

| feature | evidence |
|---|---|
| ZFX mined on NEAR with the production proof of work, checked optimistically: honest blocks never challenged; cheaters caught at every layer with one-value fraud proofs; descendants of a fraudulent block reverted; only honest blocks mint | `tests/e2e/near_opt_mining.py` (6/6) |
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

## Blocking issues for MAINNET_READY

| # | blocker | status | owner |
|---|---|---|---|
| 1 | External cryptanalysis of the S-box-chain hardness assumption | not started | external reviewers |
| 2 | Written soundness argument for the PoW | not started | ZeroFlux research |
| 3 | External security audit of `zfx-mine-opt`, the watcher and the mining proxy | not started | external auditor |
| 4 | Watcher coverage on a public network: independent watchers, alerting, challenge-window and bond sizing | watcher ships with the mining tools; parameters are testnet values | ZeroFlux core |
| 5 | Monetary schedule (reward curve, tail, supply) | the contract pays a constant testnet reward; schedules are modeled (`bench/results/tokenomics.json`, recommendation `D_smooth_decay_tail`) | project owner (decision) |
| 6 | Exactness on B200/B300, RTX 50, RTX PRO 6000 and MI300X hardware | code compiles for sm_100/sm_120/gfx942; no device reached | ZeroFlux core (needs hardware access) |
| 7 | NEAR testnet deployment of `zfx-mine-opt` | `scripts/deploy_testnet.py` ready; not run | project owner |
| 8 | Live Intents access and liquidity | ZFX works with the official Verifier; the public Message Bus needs a Partner JWT; no liquidity solicited | project owner (partner program) |
| 9 | Release signing key and signed artifacts | no key exists | project owner |
| 10 | Economic, operational and legal review | not started | project owner |

Nothing in this repository issues, distributes or markets a live token. Nothing is deployed to NEAR mainnet. On NEAR testnet, from an earlier test of the own-chain alternative: `zeroflux.testnet` (faucet-funded) and `wzfx.zeroflux.testnet`.
