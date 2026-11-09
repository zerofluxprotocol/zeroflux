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
| Strassen verdict overstated the int8 limit | hardcoded text; E4M3 test used only the max operand | verdict computed from data over every operand |

## 4. Not run here

| item | outcome | reason |
|---|---|---|
| CUDA build and run (cuBLAS int8, cuBLASLt FP8, naive kernel) on H100/H200/B200/RTX | UNSUPPORTED | no NVIDIA GPU |
| HIP build and run (hipBLAS int8, hipBLASLt FP8 E4M3FNUZ/E4M3) on MI300X/MI355X | UNSUPPORTED | no AMD GPU |
| Full nvcc / hipcc SDK builds | BLOCKED | `a private workspace` cloud workspace is paused; the script is ready at `bench/compile/compile_check.py` |
| Linux x86-64 run of the test suite | BLOCKED | same; no Docker on the host |
| Docker compose of CPU nodes | BLOCKED | Docker not installed; files written, not executed |
| Watts and joules per attempt | BLOCKED | needs root `powermetrics`; no datacenter GPU |
| Explorer visual render in a browser | not run | no headless browser installed; typecheck and HTTP checks only |
| Cross-driver and compiler-flag matrix | UNSUPPORTED | needs vendor GPUs |
