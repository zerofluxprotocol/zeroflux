# Acceptance report

Generated 2026-11-09T18:28:22Z at commit `401790e00156`. **MAINNET_READY = false.**

Regenerate: `make acceptance` (runs every target) or `ZFX_ACCEPT_REUSE=1 .venv/bin/python scripts/acceptance.py` (report only).

## Gates

| gate | status | evidence | blocked externally |
|---|---|---|---|
| 0 baseline | PASS | inventory: IMPLEMENTATION_STATUS.md (Gate 0 baseline, requirement inventory); check_target: FAIL | - |
| 1 native chain | PASS | e2e_native: 18/18 checks; contract_api: 48/48 cases; note: e2e uses PoW profile zfx-fp8c-v0.1-dev (H=256) for speed; v0.1 profile mined on GPUs below | - |
| 2 miner | PASS_WITH_BLOCKED | local: {"vectors-rust": "PASS", "vectors-cpp": "PASS", "vectors-python-numpy": "PASS", "vectors-mlx-metal": "PASS", "vectors-torch-mps": "PASS", "gpu-device-compile": ; gpu_rerun_new: {"nvidia": {"dir": "bench/results/gpu_rerun_2026-11-09_r2", "device": "cuda:NVIDIA H100 80GB HBM3:sm_90:blas-int8:int8", "correctness_steps": 13, "correctness_f; gpu_block_submissions: {"nvidia": "PASS", "amd": "PASS"}; prior_evidence: bench/results/gpu_summary.json (H100, MI355X, earlier run; not re-measured by this report) | NVIDIA B200/B300 (sm_100), RTX 50 (sm_120) and AMD MI300X (gfx942): code compiles for these targets; no such device was reachable, so no hardware result |
| 3 bridge contracts | PASS | wzfx_unit_tests: PASS; conformance_replay_access_in_e2e_bridge: ["B1 deployed wZFX: NEP-148 metadata, zero supply, committee configured", "B1/B3 1-of-3 cannot mint (on-chain rejection) and a forged member index is rejected; ; governance_upgrade_rotation: 8/8 checks (bench/results/e2e_bridge_governance.json) | - |
| 4 real two-way bridge | PASS | two_way_fault_injection: 10/10 checks; finality_reorg: 3/3 checks | - |
| 5 Intents | PASS_WITH_BLOCKED | local: 11/11 checks | public NEAR Intents Message Bus requires a Partner JWT (HTTP 403 without one); no live liquidity is solicited |
| 6 testnet deployment | PASS_WITH_BLOCKED | testnet_smoke: BLOCKED; txs: [] | NEAR_TESTNET_ACCOUNT is not set. Prerequisite: A NEAR testnet account dedicated to ZeroFlux with >= 15 testnet NEAR, its full-access key in ~/.near-credentials/testnet/<account>.json (near-cli format), and NEAR_TESTNET_ACCOUNT=<account> in the environment.  |
| 7 packaging and handoff | PASS_WITH_BLOCKED | security_test: PASS; reproducible_build: PASS; sbom: PASS; licenses: PASS; docs_missing: []; one_command_env: make localnet; schemas: schemas/zfx-node-rpc-v1.json | container images not built: Docker is not installed on this host; release artifacts unsigned: no owner release key exists; no live public testnet explorer/API: not authorized and no testnet deployment |
| 8 truthful readiness | PASS | MAINNET_READY: False; tracked_in: docs/RELEASE_READINESS.md | independent cryptanalysis of the PoW hardness assumption; external security audit (node, P2P, wZFX, relayers); owner decision on the monetary schedule (recommendation in bench/results/tokenomics.json); real liquidity for wZFX (none solicited); unmeasured GPU models (see Gate 2) |

## Targets

| target | result | PASS | FAIL | BLOCKED | commit | finished |
|---|---|---|---|---|---|---|
| `make check` | FAIL | 19 | 2 | 0 | `401790e001` (dirty) | 2026-11-09T18:11:19Z |
| `make e2e-native` | PASS | 2 | 0 | 0 | `401790e001` (dirty) | 2026-11-09T18:12:28Z |
| `make e2e-bridge` | PASS | 3 | 0 | 0 | `401790e001` (dirty) | 2026-11-09T18:20:34Z |
| `make e2e-intents` | PASS | 1 | 0 | 0 | `401790e001` (dirty) | 2026-11-09T18:21:51Z |
| `make security-test` | PASS | 10 | 0 | 0 | `401790e001` (dirty) | 2026-11-09T18:27:45Z |
| `make gpu-test` | PASS_WITH_BLOCKED_EXTERNAL | 5 | 0 | 3 | `401790e001` (dirty) | 2026-11-09T18:28:22Z |
| `make testnet-smoke` | PASS_WITH_BLOCKED_EXTERNAL | 0 | 0 | 1 | `401790e001` (dirty) | 2026-11-09T18:28:22Z |

## Steps

### make check

| step | result | seconds | command or reason |
|---|---|---|---|
| rustfmt | FAIL | 0.2 | `cargo fmt --all --check && (cd bridge/near && cargo fmt --all --check)` |
| clippy-workspace | PASS | 0.2 | `cargo clippy --release --workspace --all-targets -q -- -D warnings` |
| clippy-near-contracts | PASS | 0.7 | `cd bridge/near && cargo clippy -q --all-targets -- -D warnings` |
| cargo-test-workspace | PASS | 53.5 | `cargo test --release -q --workspace` |
| cargo-test-near-contracts | PASS | 1.1 | `cd bridge/near && cargo test -q` |
| wasm-build-wzfx | PASS | 5.9 | `cd bridge/near/wzfx && cargo near build non-reproducible-wasm` |
| wasm-build-test-ft | PASS | 3.9 | `cd bridge/near/test-ft && cargo near build non-reproducible-wasm` |
| wasm-build-pow-verifier | PASS | 0.4 | `cd bridge/near/pow-verifier && cargo near build non-reproducible-wasm --no-abi` |
| build-release | PASS | 4.6 | `cargo build --release -q` |
| vectors-rust | PASS | 0.3 | `./target/release/zpu-cli selftest` |
| vectors-consensus-v2 | PASS | 15.6 | `cargo test --release -q -p zfx-verify --test consensus_v2` |
| cpp-build | PASS | - | `make -C miner/cuda cpu -s` |
| vectors-cpp | PASS | - | `./miner/cuda/build/zpu-miner selftest tests/vectors/fp8c_v0.1.json` |
| vectors-python-numpy | PASS | 0.5 | `.venv/bin/python miner/python/zpu_miner.py selftest tests/vectors/fp8c_v0.1.json --backend numpy` |
| stress-python-numpy | PASS | 4.6 | `.venv/bin/python miner/python/zpu_miner.py stress --backend numpy` |
| vectors-mlx-metal | PASS | 0.8 | `.venv/bin/python miner/python/zpu_miner.py selftest tests/vectors/fp8c_v0.1.json --backend mlx` |
| vectors-torch-mps | PASS | 1.5 | `.venv/bin/python miner/python/zpu_miner.py selftest tests/vectors/fp8c_v0.1.json --backend torch --device mps` |
| gpu-device-compile | PASS | 8.4 | `CLANG='.venv/bin/python -m ziglang clang' bench/compile/device_check.sh` |
| python-lint | FAIL | 0.4 | `uvx pyflakes tests/e2e deploy/localnet.py testnet/zfxnet.py scripts bridge/intents/zfx_intents miner/python bench/gpu` |
| explorer-typecheck | PASS | 0.7 | `cd explorer && npx --no-install tsc -p . --noEmit` |
| intents-nep413-conformance | PASS | 14.0 | `cargo test --release -q -p zfx-bridge --test nep413_conformance` |

### make e2e-native

| step | result | seconds | command or reason |
|---|---|---|---|
| native-3-nodes-1000-blocks | PASS | 59.1 | `.venv/bin/python tests/e2e/native.py` |
| contract-api-schema-conformance | PASS | 9.5 | `.venv/bin/python tests/contract_api/test_contract_api.py` |

### make e2e-bridge

| step | result | seconds | command or reason |
|---|---|---|---|
| bridge-100-two-way-fault-injection | PASS | 281.0 | `.venv/bin/python tests/e2e/bridge.py` |
| bridge-finality-reorg | PASS | 65.6 | `.venv/bin/python tests/e2e/bridge_reorg.py` |
| bridge-governance-recovery | PASS | 139.1 | `.venv/bin/python tests/e2e/bridge_governance.py` |

### make e2e-intents

| step | result | seconds | command or reason |
|---|---|---|---|
| intents-local-solver-settlement | PASS | 77.5 | `.venv/bin/python tests/e2e/intents.py` |

### make security-test

| step | result | seconds | command or reason |
|---|---|---|---|
| fuzz-parsers | PASS | 143.0 | `cargo test --release -q -p zfx-node --test fuzz_parsers -- --nocapture` |
| node-integrity-crash-corruption-backup | PASS | 1.0 | `cargo test --release -q -p zfx-node --test node_integrity` |
| chain-rules-adversarial | PASS | 6.6 | `cargo test --release -q -p zfx-node --test chain_rules` |
| consensus-v2-adversarial | PASS | 30.9 | `cargo test --release -q -p zfx-verify --test consensus_v2` |
| wzfx-contract-negative | PASS | 3.1 | `cd bridge/near && cargo test -q -p wzfx` |
| miner-asan-ubsan | PASS | 2.5 | `make -C miner/cuda sanitize-test` |
| research-attacks | PASS | 0.8 | `./target/release/zfx-research attacks --quick --out /tmp/zfx-attacks` |
| cargo-audit-workspace | PASS | 1.4 | `cargo audit --deny warnings` |
| cargo-audit-near-contracts | PASS | 1.1 | `cd bridge/near && cargo audit --ignore RUSTSEC-2026-0387 --ignore RUSTSEC-2024-0449` |
| release-licenses-sbom-reproducible | PASS | 162.6 | `.venv/bin/python scripts/release_check.py` |

### make gpu-test

| step | result | seconds | command or reason |
|---|---|---|---|
| gpu-device-compile-sm90-sm100-sm120-gfx942-gfx950 | PASS | 15.2 | `CLANG='.venv/bin/python -m ziglang clang' bench/compile/device_check.sh` |
| apple-mlx-selftest | PASS | 1.1 | `.venv/bin/python miner/python/zpu_miner.py selftest tests/vectors/fp8c_v0.1.json --backend mlx` |
| apple-mlx-stress | PASS | 5.6 | `.venv/bin/python miner/python/zpu_miner.py stress --backend mlx` |
| apple-mps-selftest | PASS | 2.0 | `.venv/bin/python miner/python/zpu_miner.py selftest tests/vectors/fp8c_v0.1.json --backend torch --device mps` |
| apple-mps-stress | PASS | 9.2 | `.venv/bin/python miner/python/zpu_miner.py stress --backend torch --device mps` |
| nvidia-local | BLOCKED | - | no NVIDIA GPU/driver on this host (nvidia-smi absent) |
| amd-local | BLOCKED | - | no AMD GPU/ROCm on this host (rocm-smi/amd-smi absent) |
| remote-h100-mi355x-cloud | BLOCKED | - | remote GPU rerun is opt-in (ZFX_GPU_RUNNER); it rents GPU sandboxes |

### make testnet-smoke

| step | result | seconds | command or reason |
|---|---|---|---|
| near-testnet-deploy-and-receipts | BLOCKED | - | NEAR_TESTNET_ACCOUNT is not set |

Full output of every step: `bench/results/gates/logs/<target>/<step>.log`. Raw experiment and e2e results: `bench/results/`.
