# ZeroFlux (ZFX)

A proof-of-work chain whose mining primitive is the computation AI inference hardware is built for: batched FP8/INT8 GEMMs with FP32/INT32 accumulation. Native ZFX uses hash-based SLH-DSA signatures, P2MR addresses, hash-only verification and an encrypted, authenticated P2P transport. A 2-of-3 committee bridge carries ZFX to NEAR as wZFX, which trades through NEAR Intents.

```
PRIVATE_TESTNET_READY = true      MAINNET_READY = false      testnet only, no monetary value
```

> **Bridge trust model.** The v1 bridge is a 2-of-3 trusted custodian. Any two committee members together can steal all locked ZFX or mint unbacked wZFX. Details: `docs/BRIDGE_TRUST_MODEL.md`.

## One-command targets

| command | what it runs |
|---|---|
| `make check` | rustfmt, clippy `-D warnings`, all Rust and contract unit tests, WASM builds, golden vectors in Rust/C++/Python/MLX/MPS, GPU device compile for sm_90/100/120 and gfx942/950, Python lint, explorer typecheck, NEP-413 conformance |
| `make e2e-native` | 3 nodes, 1000 mined blocks, restarts, encrypted P2P, wallet transfers, supply audit, explorer receipts |
| `make e2e-bridge` | native chain + NEAR sandbox: 100 two-way transfers under fault injection, finality and reorg cases, governance (pause, rotation, upgrade) and recovery |
| `make e2e-near` | ZFX mined natively on NEAR (proof of work checked on-chain) and a ZeroFlux PoW light client on NEAR; see `docs/NEAR_SETTLEMENT.md` |
| `make e2e-intents` | official Verifier, local Message Bus, test solver: quotes, settlement both ways, expiry, liquidity, duplicates, refunds |
| `make security-test` | parser fuzzing, crash/corruption/backup tests, adversarial consensus tests, ASan+UBSan miner, attack experiments, `cargo audit`, SBOM, license gate, reproducible build |
| `make gpu-test` | every GPU backend on this host; missing NVIDIA/AMD hardware reported as BLOCKED (remote rerun: set `ZFX_GPU_RUNNER`) |
| `make testnet-smoke` | real NEAR testnet, only with an explicitly configured ZeroFlux account (`NEAR_TESTNET_ACCOUNT`) |
| `make acceptance` | all of the above, then `ACCEPTANCE_REPORT.md` and `.json` |
| `make localnet` | the full local stack for manual use |

Each target writes per-step evidence to `bench/results/gates/<target>.json` and exits non-zero if a required step fails.

Prerequisites: Rust 1.90+, `cargo-near`, a C++17 compiler, Python 3.11+ with `uv`, Node.js (explorer typecheck). Setup: `uv venv .venv && uv pip install --python .venv/bin/python numpy jsonschema websockets ziglang` (add `torch`, `mlx` for Apple GPU backends). The NEAR sandbox needs liblzma; see `docs/OPERATOR_RUNBOOK.md`.

## Where things are

| spec module | path | contents |
|---|---|---|
| `node/` | `crates/zfx-node`, consensus in `crates/zfx-verify` | full node, state, P2P, JSON-RPC, persistence, explorer API |
| `miner/` | `miner/cli`, `miner/cuda`, `miner/python` | Rust client; C++ CPU/CUDA/HIP miner; NumPy/PyTorch/MLX miner |
| `wallet/` | `crates/zfx-wallet` | SLH-DSA P2MR wallet, backup/restore, bridge deposit |
| `explorer/` | `explorer` | read-only UI served by the node |
| `bridge/native/` | `crates/zfx-verify/src/bridge.rs`, `state.rs` | lockbox, 2-of-3 release/admin enforced by consensus, inclusion proofs |
| `bridge/near/` | `bridge/near/wzfx` | NEP-141/145/148 wZFX contract, governance, unit tests |
| `bridge/relayers/` | `bridge/relayers` | relayers, coordinator, reconciliation, operator CLI `zfx-bridge` |
| `bridge/intents/` | `bridge/intents` | Intents adapter, test solver, Verifier fetch, NEP-413 fixtures |
| `deploy/` | `deploy/localnet.py`, `deploy/gpu`, `Dockerfile`, `docker-compose.yml` | local topology, GPU containers |
| `tests/e2e/` | `tests/e2e` | the e2e scripts behind the make targets, plus `testnet_smoke.py` |
| schemas | `schemas/` | JSON-RPC schema and captured fixtures |
| research | `crates/zfx-research`, `crates/zfx-stark-proto`, `bridge/near/pow-verifier` | algorithm, tokenomics, fork resilience, STARK and on-chain PoW feasibility |

## How the PoW works

Every attempt derives a 64-value input row from `SHAKE256(SHA256(header) || nonce)`. The row goes through 64 -> 4096 -> 4096 -> 4096 -> 4096 -> 256 integer layers whose weights are regenerated every 32 blocks, with an AES-S-box requantization between layers. A block is valid when `SHA256(output)` is below the target. Operands are integers in {±1..±16}, so int8 and FP8 tensor cores and every CPU compute the same bits: all sums stay below 2^24. Miners batch thousands of nonces into `[batch x 4096] x [4096 x 4096]` GEMMs; a node verifies a block on one CPU core with no GPU runtime. Details: `docs/CONSENSUS.md`, rationale: `docs/ALGORITHM_DECISION.md`.

## Documents

`docs/` holds ALGORITHM_DECISION, CONSENSUS, THREAT_MODEL, TOKEN_ACCOUNTING, BRIDGE_TRUST_MODEL, CONTRACT_API, INTENTS_INTEGRATION, TEST_REPORT, HARDWARE, BENCHMARKS, RELEASE_READINESS, OPERATOR_RUNBOOK, NEAR_SETTLEMENT and the specifications. `IMPLEMENTATION_STATUS.md` and `ACCEPTANCE_REPORT.md` at the root track the gates.

License: Apache-2.0.
