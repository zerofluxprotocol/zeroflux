# ZeroFlux (ZFX)

A proof-of-work chain whose mining primitive is the computation AI inference hardware is built for: batched FP8/INT8 GEMMs with FP32/INT32 accumulation. The chain uses SLH-DSA signatures, P2MR addresses, and hash-only verification.

```
PRIVATE_TESTNET_READY = true      MAINNET_READY = false      no monetary value
```

## What is here

| path | component | language |
|---|---|---|
| `crates/zfx-verify` | consensus: canonical header, ZFX-FP8C PoW, SLH-DSA/P2MR transactions, LWMA difficulty, test-vector generator | Rust |
| `crates/zfx-node` | full node: chain state, fork choice, reorgs, mempool, P2P, miner/wallet/explorer HTTP API, metrics | Rust |
| `crates/zfx-wallet` | CLI wallet (SLH-DSA, P2MR addresses) | Rust |
| `miner/cli` | `zpu-cli`: connects any miner backend to a node, re-verifies GPU output on the CPU | Rust |
| `miner/cuda` | `zpu-miner`: independent C++ implementation, CPU + one CUDA/HIP source (NVIDIA Hopper/Blackwell/RTX, AMD MI300X/MI355X) | C++ / CUDA / HIP |
| `miner/python` | `zpu_miner.py`: third independent implementation, NumPy / PyTorch (CUDA, ROCm, MPS) / MLX (Metal) | Python |
| `crates/zfx-research` | algorithm-selection experiments: candidates A-E, accumulation models, transcript forgeries, attacks | Rust |
| `explorer` | block explorer served by the node at `/` | TypeScript |
| `testnet` | multi-node launcher and 11-check integration scenario | Python |
| `bench`, `tests/vectors` | benchmark runners, raw results, published consensus vectors | |

## Quick start (CPU, about 2 minutes)

```
cargo build --release
make -C miner/cuda cpu
uv venv .venv && uv pip install --python .venv/bin/python numpy   # add torch / mlx for GPU backends
tests/run_all.sh                                  # every suite, PASS/FAIL per line
python3 testnet/zfxnet.py up                      # 3 nodes + Rust, C++ and Python miners; explorer at http://127.0.0.1:19443/
```

By hand:

```
./target/release/zfx-wallet new                                   # prints zfx1... (SLH-DSA P2MR address)
./target/release/zfx-node --devnet --data-dir /tmp/zfx1 --explorer-dir explorer/dist
./target/release/zpu-cli mine --node http://127.0.0.1:9441 --address zfx1...
./target/release/zpu-cli mine --node http://127.0.0.1:9441 --address zfx1... \
    --backend exec --exec ".venv/bin/python miner/python/zpu_miner.py --backend mlx"    # Apple GPU
./target/release/zfx-wallet send --to zfx1... --amount 100000
```

GPU miners: `make -C miner/cuda cuda` or `make -C miner/cuda hip`, then follow the bring-up checklist in `docs/HARDWARE.md`.

## How the PoW works (one paragraph)

Every attempt derives a 64-value input row from `SHAKE256(SHA256(header) || nonce)`. The row goes through 64 -> 4096 -> 4096 -> 4096 -> 4096 -> 256 integer layers whose weights are regenerated every 32 blocks, with an AES-S-box requantization between layers. A block is valid when `SHA256(output)` is below the target. Operands are integers in {±1..±16}, so every GPU's int8 or FP8 tensor cores and every CPU compute the same bits: all sums stay below 2^24. Miners batch thousands of nonces into ordinary `[batch x 4096] x [4096 x 4096]` GEMMs, while a node verifies a block in about 1 ms on one CPU core with no GPU runtime. Details: `docs/CONSENSUS.md`. The reasons for this design over the four mandated candidates are in `docs/ALGORITHM_DECISION.md`.

## Documents

- `docs/ALGORITHM_DECISION.md`: candidates A-E, measurements, scored gates, selection
- `docs/CONSENSUS.md`: byte-level protocol
- `docs/THREAT_MODEL.md`: attacks tried (including two that broke rejected candidates), assumptions, open risks
- `docs/BENCHMARKS.md` and `bench/results/`: per-device numbers and raw JSON
- `docs/TEST_REPORT.md`: commands, environment, PASS/FAIL/UNSUPPORTED/BLOCKED
- `docs/RELEASE_READINESS.md`: blockers and owners
- `docs/HARDWARE.md`: which GEMM path per GPU, bring-up checklist
- `docs/SPEC_v0.1.md`, `docs/SPEC_AMENDMENT_A.md`: the specification

## Status in one table

| | result |
|---|---|
| Bit-exact: Rust, C++, Python; CPU, Apple GPU (MLX, MPS) | PASS |
| NVIDIA H100 / AMD MI355X, int8 + FP8 | PASS bit-exact; 8.08 M and 13.08 M attempts/s, about 90 µJ per attempt on both |
| Multi-node testnet, 11 network checks, two PoW profiles | PASS |
| Independent cryptanalysis, audit, transport encryption, public testnet | not done (see RELEASE_READINESS.md) |


License: Apache-2.0 (see LICENSE).
