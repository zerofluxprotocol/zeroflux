# Test report

Date: 2026-11-09. Host: Apple M5 Pro, macOS 26.6 (Darwin 25.6, arm64), Rust 1.99.1, Apple clang 21, Python 3.12.15, NumPy 2.5.4, PyTorch 2.15.0 (MPS), MLX 0.32.4, Node 24.22, TypeScript 5.x, zig 0.17.1 (bundled clang 22.1.8, compile checks only).

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

Driver: `.venv/bin/python bench/gpu/cloud_gpu_run.py` (ephemeral cloud GPU machines, obfuscated names, TTL plus auto-stop backstops). It runs `bench/gpu/box_run.sh` inside each box. Every sandbox id is ledgered in `bench/results/gpu_ledger.json`. The API key has no delete permission, so teardown is stop (Cloud deletes stopped GPU sandboxes), then a 404 check. 12 sandboxes in total (6 H100, 6 MI355X), all verified absent afterwards.

## 4. Not run here

| item | outcome | reason |
|---|---|---|
| CUDA run on H200, B200, B300, RTX 4090/5090, RTX PRO 6000 | UNSUPPORTED | not measured yet (available to rent; same script) |
| HIP run on MI300X (gfx942, E4M3FNUZ path) | UNSUPPORTED | not offered by the provider used |
| Docker compose of CPU nodes | BLOCKED | Docker not installed; files written, not executed |
| Watts and joules per attempt on Apple | BLOCKED | needs root `powermetrics` (GPU joules measured on H100 and MI355X) |
| Explorer visual render in a browser | not run | no headless browser installed; typecheck and HTTP checks only |
| Cross-driver and compiler-flag matrix | partial | one driver/toolkit per vendor: CUDA 12.8 (PyTorch 2.11 image) and ROCm 10.0 (vLLM image) |
