# Release readiness

```
PRIVATE_TESTNET_READY = true
MAINNET_READY         = false
```

`PRIVATE_TESTNET_READY` means a multi-node private network with no monetary value runs, mines with three independent miner implementations, transfers with hash-based signatures, reorgs, rejects invalid blocks and recovers from crashes. All of this is shown by `bench/results/testnet_integration*.json` on two PoW profiles.

## Verified features

| feature | evidence |
|---|---|
| Selected PoW implemented end to end (ZFX-FP8C) | `crates/zfx-verify/src/pow.rs`, CONSENSUS.md, vectors |
| Bit-exact canonical arithmetic: Rust, C++, Python; CPU, Apple GPU (MPS, MLX), NVIDIA H100, AMD MI355X (int8 and FP8) | TEST_REPORT.md, `gpu_summary.json` |
| GPU miner for NVIDIA and AMD from one source: H100 8.08 M attempts/s (85.9 µJ), MI355X 13.05 M/s (94.6 µJ) | `miner/cuda/src/backend_gpu.cu`, BENCHMARKS.md 1b |
| CPU verifier with no GPU runtime | `zfx-node` links only zfx-verify; 0.985 ms per PoW |
| SLH-DSA signatures (128s/192s) and P2MR addresses | `tx.rs` tests, `signature_costs.json` |
| Multi-node P2P, reorgs, LWMA, MTP, mempool, double-spend rejection, crash recovery | `chain_rules.rs`, testnet scenario |
| Explorer, wallet, mining client, metrics | `explorer/`, `zfx-wallet`, `zpu-cli`, `/metrics` |
| Candidate study A to E, attacks, decision | ALGORITHM_DECISION.md, THREAT_MODEL.md |

## Blocking issues for MAINNET_READY

| # | blocker | status | owner |
|---|---|---|---|
| 1 | Exactness and throughput on every target GPU | **H100 and MI355X PASS** (int8 + FP8 + portable, 2^20 stress, 8.1 M/s and 13.1 M/s); B200, B300, RTX PRO 6000, RTX 4090/5090 and MI300X not yet run | ZeroFlux core (same Cloud script; MI300X needs another provider) |
| 2 | Full nvcc/hipcc SDK builds plus Linux x86-64 tests | **PASS** (CUDA 12.8 sm_90, ROCm 10.0 gfx950, cargo test 22/22 on x86-64) | done |
| 3 | External cryptanalysis of the S-box-chain hardness assumption (Gate 3) | not started | external reviewers |
| 4 | Written soundness argument for the PoW (Gates 3 and 4) | not started | ZeroFlux research |
| 5 | DoS economics of invalid-block spam (1 ms each) and initial sync cost | open | ZeroFlux core |
| 6 | P2P transport encryption and peer authentication (ML-KEM-768 hybrid) | not built | ZeroFlux core |
| 7 | Eclipse/Sybil resistance (address gossip, peer diversity) and undo-based reorgs instead of O(n) replay | not built | ZeroFlux core |
| 8 | Audit of the SLH-DSA integration, P2MR and wallet key derivation; encrypted wallet files | not started | external auditor |
| 9 | int8 vs FP8 Strassen asymmetry (up to 1.49x) and ASIC economics (spec Q4) | documented, unresolved | ZeroFlux research |
| 10 | Mainnet block time, retarget, reward schedule, supply | intentionally not set | project owner and governance |
| 11 | Economic, operational and legal review (Gate 6) | not started | project owner |
| 12 | Geographically distinct public testnet with diverse miners (Gate 5) | not started | ZeroFlux ops after #1 and #6 |

## Development gates (spec section 9)

| gate | status | evidence / missing |
|---|---|---|
| 1 Deterministic arithmetic | PASS on CPU (arm64, x86-64), Apple GPU, NVIDIA H100, AMD MI355X | remaining GPU models in blocker 1 |
| 2 Mining prototype (two independent miners, identical accepted blocks on CPU nodes) | PASS | testnet check 2: Rust and C++ (plus Python) miners on one chain |
| 3 Cryptographic research | FAIL (not yet passed) | internal attacks done; independent review missing |
| 4 Verification | partial: practical asymmetry (one attempt, 1 ms), no audited implementation | blockers 4, 5, 8 |
| 5 Public testnet | not started | blocker 12 |
| 6 Public-value network | not started | blocker 11 |
