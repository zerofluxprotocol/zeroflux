# Test report

Date: 2026-11-09. Host: Apple M5 Pro, macOS 26.6 (arm64), Rust 1.99.1, Apple clang 21, Python 3.12, NumPy 2.5, PyTorch 2.15 (MPS), MLX 0.32, cargo-near 0.23, NEAR sandbox (nearcore, protocol 88). GPU hosts: ephemeral Cloud sandboxes with H100 80GB (CUDA 12.8) and MI355X (ROCm 10.0).

The authoritative, regenerated matrix is `ACCEPTANCE_REPORT.md` (per step: command, result, seconds; full logs under `bench/results/gates/logs/`). This page summarizes it.

## ZeroFlux on NEAR (current design)

| test | result |
|---|---|
| `tests/e2e/near_opt_mining.py`: mining with the production proof of work, checked optimistically | 7/7: a bond below a pending block's storage cost is refused; honest blocks never challenged; cheaters caught at layer 0, a hidden layer and the outputs; descendants reverted with bonds refunded; false challenge rejected; only honest blocks mint |
| `tests/e2e/near_intents.py`: ZFX on NEAR Intents | 5/5: mined ZFX deposited into the official Verifier and swapped both ways; supply fully accounted for |
| `tests/e2e/near_mining.py`, `near_light_client.py`: alternatives on NEAR | 8/8, 6/6 |
| `make localnet` quickstart, run by hand | 32 blocks mined in 45 s; the watcher validated all 32; minted supply equals finalized blocks |

Run with `make e2e-near`. The rest of this report covers the own-chain alternative and the proof of work itself.

## Commands

```
make check            make e2e-native      make e2e-bridge      make e2e-intents
make security-test    make gpu-test        make testnet-smoke   make acceptance
ZFX_GPU_RUNNER=/path/to/runner make gpu-test   # optional: rents ephemeral GPU machines
```

## Results (commit 401790e and later)

| target | result | content |
|---|---|---|
| `make check` | PASS 26/26 | rustfmt, clippy `-D warnings` (2 workspaces), workspace unit tests, 6 WASM builds, golden vectors in Rust, C++, NumPy, MLX (Metal), PyTorch MPS, GPU device compile (5 targets), pyflakes, explorer typecheck, NEP-413 conformance |
| `make e2e-native` | PASS 2/2 | 18/18 network checks at 1,033 blocks on 3 nodes (dev PoW profile); contract API 48/48 cases against a live node |
| `make e2e-bridge` | PASS 3/3 | 10/10 (100 two-way transfers under fault injection, exact reconciliation, per-user conservation); reorg 3/3; governance and recovery 8/8 |
| `make e2e-intents` | PASS 1/1 | 11/11 on the official Verifier defuse/v0.4.2 |
| `make security-test` | PASS 10/10 | fuzzing (8 parsers, 20,000 inputs each, 0 panics), crash/corruption/backup, adversarial consensus, contract negatives, ASan+UBSan miner, attack experiments, `cargo audit` x2, SBOM + license gate + reproducible build (identical hashes from two clean builds) |
| `make gpu-test` | PASS 5, BLOCKED 3 | device compile and Apple GPU (MLX, MPS) selftest + stress pass; local NVIDIA/AMD absent; remote rerun opt-in (run separately, below) |
| `make testnet-smoke` | BLOCKED 1 | `NEAR_TESTNET_ACCOUNT` not set (see ACCEPTANCE_REPORT Gate 6) |

GPU rerun 2 (`bench/results/gpu_rerun_2026-11-09_r2/`): H100 34/34 and MI355X 34/34 steps PASS, covering selftest and 2^20 stress for every GEMM path (int8, FP8, portable), PyTorch backends, Rust tests on x86-64, and real v0.1 mining: 42 (H100) and 57 (MI355X) GPU-mined blocks accepted by a CPU node and re-validated by an independent CPU node. Numbers: `docs/HARDWARE.md`.

## Defects found by these tests and fixed

| defect | found by | fix |
|---|---|---|
| `zfx_node_get_block {"hash": "00..00"}` returned the genesis block (all-digit hash parsed as a height) | contract API negative case | 64-char keys are always hashes |
| GPU mining capped at 29k attempts/s (2,000-attempt jobs) | GPU rerun 1 block submissions (0 blocks) | adaptive job sizing in `zpu-cli` |
| relayer could sign a second, different payout for an id after a restart | runbook review | persistent anti-equivocation guard, unit-tested |
| a halted coordinator had no supported resume path | runbook review | `--resume-after-incident`, refuses unless books are clean (e2e tested both ways) |
| a replayed duplicate mint injected by the chaos thread would only have been logged | lint (unused variable) | Gate 4 check now fails on it and counts completed round trips |
| relayer/contract digest equality was only implied by e2e | doc review | shared golden vectors in both crates |
| `zfx-node --help`, `zpu-cli mine --help` started processes | manual use | usage on `--help` for all CLIs |
| unmaintained `wee_alloc` in the wZFX WASM | `cargo audit` | near-sdk default features off; bridge e2e rerun on the new WASM |

## Appendix: v0.1 detail (earlier run)

Outcome vocabulary: PASS, FAIL, UNSUPPORTED (hardware not present), BLOCKED (cannot run here, with the reason).

## 1. Commands

```
tests/run_all.sh --with-testnet          # every suite below except benchmarks
bench/run_benchmarks.sh                  # miners: selftest, stress, throughput
./target/release/zfx-research all        # candidate experiments and attacks
python3 testnet/zfxnet.py scenario --profile v01 --out bench/results/testnet_integration_v01.json
CLANG="python3 -m ziglang clang" bench/compile/device_check.sh
```

## 2. Results

| suite | what it checks | result |
|---|---|---|
| `cargo test` zfx-verify (13 tests) | U256, header roundtrip, domain balance and FP32 exactness, requant balance, order independence, nonce binding, epoch anchors, LWMA, MTP, SLH-DSA P2MR sign/verify/tamper/wrong-leaf/malleability, XMSS rejection, **vector drift guard** | PASS |
| `cargo test` zfx-node `chain_rules` (6 tests) | linear chain and rewards; reorg to more work and first-seen tie; invalid PoW / easier target / wrong version / future timestamp / tx_root mismatch / coinbase overpay / timestamp <= MTP; LWMA enforcement; mempool double spend, miner API, stale work expiry, reorg tx return; disk recovery with a truncated final line, config mismatch refusal | PASS |
| `cargo test` zfx-research (3 tests) | E4M3 and BF16 codecs, C1 forgery accepted by verifier | PASS |
| `zpu-cli selftest` | Rust, 10 attempt vectors | PASS 10/10 |
| `zpu-miner selftest` | C++ (independent), 278 checks | PASS 278/278 |
| `zpu_miner.py selftest` numpy / torch-cpu / torch-mps / mlx | Python (independent), 278 checks each; MPS and MLX run on the Apple GPU | PASS x4 |
| `zpu_miner.py stress` numpy / torch-cpu / torch-mps / mlx | 512x4096x4096 GEMM incl. rows with sum = ±2^20, vs int64 | PASS x4 (0 mismatches) |
| `device_check.sh` | all 6 GPU kernels for sm_90, sm_100, sm_120, gfx942, gfx950 | PASS 5/5 targets |
| explorer `tsc --noEmit` | TypeScript strict typecheck | PASS |
| explorer served by node | `/`, `/main.js`, `/v1/blocks`, `/metrics` | PASS (curl) |
| testnet scenario, dev profile | 11 network checks, below | PASS 11/11 |
| testnet scenario, v0.1 profile | same 11 checks on H=4096 | PASS 11/11 |
| H100 C++ `zpu-miner-cuda` (cuBLAS int8, cuBLASLt FP8, portable) | selftest 278/278 + 2^20 stress + bench, real GPU | PASS x3 |
| MI355X C++ `zpu-miner-hip` (hipBLAS int8, hipBLASLt FP8, portable) | same | PASS x3 |
| H100 and MI355X PyTorch fp32 / int8 / fp8 | selftest + stress | PASS x6 per GPU |
| Linux x86-64 (H100 host CPU) | `cargo test` (22), Rust selftest, C++ CPU selftest, nvcc-built binary's CPU path | PASS |
| hostname peers | `--peer localhost:port` resolves IPv4 after IPv6 fails, single connection | PASS |

Testnet scenario checks (real processes: 3 Rust nodes; miners in Rust, C++ and Python):

1. Nodes 1 and 2 agree on block hashes.
2. Gate 2: blocks from the Rust and C++ miners are both accepted on the shared CPU-verified chain (dev: 8 Rust and 16 C++ blocks).
3. An SLH-DSA P2MR transfer is accepted (signed in 0.9 to 1.0 s, 8,018 bytes).
4. The transfer is confirmed on the peer node with exact balances.
5. A replayed transaction is rejected (bad nonce).
6. An invalid-PoW block pushed over P2P is rejected with `AboveTarget` and the peer is disconnected.
7. Restart recovery: the node reloads its own chain from disk before syncing.
8. Partition heal: the isolated chain reorgs onto the heavier chain (depth 9 on dev).
9. SIGKILL crash, restart and resync.
10. A Python-miner block is accepted on the main chain.
11. LWMA moves difficulty off the genesis target.

## 3. Failures found and fixed during testing

| found | cause | fix |
|---|---|---|
| Rust miner won 0 of 17 early blocks | jobs ran to completion after a winner; easy genesis target meant constant stale work | `stop_on_found` in all three miners; harder scenario genesis |
| Partition never healed | the invalid-block test IP-banned 127.0.0.1, which every local peer shares | loopback peers are disconnected, not banned |
| Rust CPU path 4.6 to 5.4x slower than C++ | `dot_i8` with i16 chunks vectorized badly | widening 4-accumulator dot; 300 to 1,352 attempts/s on 2 threads |
| Scenario checks passed or failed for the wrong reason | the recovery check read post-sync state; the LWMA check compared against a stale string | rewritten to read the recovery log and the configured genesis target |
| `--peer hostname:port` never connected | parsed as a literal SocketAddr; only the first resolved address was tried | resolve plus try all addresses |
| nvcc: domain strings "undefined in device code" | host `static const` arrays referenced from device code (clang accepts, nvcc rejects) | `__constant__` copies in device passes |
| hipcc: `__forceinline__` unknown in .cpp sources | hipcc does not auto-include the HIP runtime header | `hd.h` includes the vendor runtime |
| PyTorch int8 shape error | wrong operand orientation for `torch._int_mm` | pass `w.t()` |
| hipBLAS/hipBLASLt failed to load kernels | pip-packaged ROCm SDK keeps Tensile code objects outside its lib dir | miner locates them and sets `ROCBLAS_TENSILE_LIBPATH`/`HIPBLASLT_TENSILE_LIBPATH` for itself only |
| H100 at 1.84 M/s with GEMMs at 60% of peak | data-dependent `__constant__` S-box reads serialize per warp (79% of batch time) | shared-memory table + vectorized loads: 8.08 M/s |
| Strassen verdict overstated the int8 limit | hardcoded text; E4M3 test used only the max operand | verdict computed from data over every operand |

## 3b. GPU runs

Driver: an external runner (`ZFX_GPU_RUNNER`) that rents ephemeral GPU machines with auto-stop backstops, and deletes them after each run. It runs `bench/gpu/box_run.sh` inside each box. Every sandbox id is ledgered in `bench/results/gpu_ledger.json`. The API key has no delete permission, so teardown is stop (Cloud deletes stopped GPU sandboxes), then a 404 check. 12 sandboxes in total (6 H100, 6 MI355X), all verified absent afterwards.

## 4. Not run here

| item | outcome | reason |
|---|---|---|
| CUDA run on H200, B200, B300, RTX 4090/5090, RTX PRO 6000 | UNSUPPORTED | not measured yet (available to rent; same script) |
| HIP run on MI300X (gfx942, E4M3FNUZ path) | UNSUPPORTED | not offered by the provider used |
| Docker compose of CPU nodes | BLOCKED | Docker not installed; files written, not executed |
| Watts and joules per attempt on Apple | BLOCKED | needs root `powermetrics` (GPU joules measured on H100 and MI355X) |
| Explorer visual render in a browser | not run | no headless browser installed; typecheck and HTTP checks only |
| Cross-driver and compiler-flag matrix | partial | one driver/toolkit per vendor: CUDA 12.8 (PyTorch 2.11 image) and ROCm 10.0 (vLLM image) |
