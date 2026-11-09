# Hardware guide

The consensus function is defined on integers in {±1..±16}, with every accumulation below 2^24. Any device whose GEMM has an exact int32 or a true FP32 accumulator gets the canonical bits. The miner chooses the storage format per device. A wrong path cannot split consensus, because nodes verify on the CPU. It can only waste the miner's work, and `zpu-cli` detects that (every winner plus random spot checks are re-verified on the CPU).

## Which path per GPU

| GPU | arch | recommended path | alternative | FP8 encoding | notes |
|---|---|---|---|---|---|
| H100 / H200 | sm_90 | `--gemm blas-int8` (cuBLAS int8 -> int32, exact) | `lt-fp8` | OCP E4M3 | Hopper FP8 MMA accumulation precision is the known hazard; run `selftest` + `stress` before trusting `lt-fp8` |
| B200 | sm_100 | `blas-int8` | `lt-fp8` | OCP E4M3 | measure both |
| B300 (Blackwell Ultra) | sm_103 | `lt-fp8` | `blas-int8` (slow) | OCP E4M3 | dense INT8 is about 0.15 to 0.19 POPS vs 4.5 on B200 (traded for NVFP4), so FP8 is the only fast path and its exactness under `stress` decides whether B300 can mine efficiently |
| RTX 4090 / 5090, RTX PRO 6000 | sm_89 / sm_120 | `blas-int8` | `lt-fp8` | OCP E4M3 | consumer FP8 MMA with FP16 accumulate is inexact (26% of rows diverge in our model); use FP32-output paths only |
| MI300X | gfx942 | `blas-int8` (hipBLAS) | `lt-fp8` (hipBLASLt) | **E4M3FNUZ** (bias 8, no -0) | the miner selects FNUZ from `gcnArchName` |
| MI355X | gfx950 | `blas-int8` | `lt-fp8` | OCP E4M3 | |
| Apple M-series | Metal | `zpu_miner.py --backend mlx` | `--backend torch --device mps` | fp32 GEMM | measured bit-exact |
| any CPU | | `zpu-cli --backend cpu` or `zpu-miner --backend cpu` | NumPy fp32 BLAS | | reference |

## Build

```
make -C miner/cuda cpu                                   # any OS
make -C miner/cuda cuda CUDA_ARCH="90 100 120"           # needs CUDA 12.8+ (sm_120)
make -C miner/cuda hip  HIP_ARCH="gfx942 gfx950"         # needs ROCm 6.4+ with hipBLAS and hipBLASLt
```

## Bring-up checklist on a new GPU (in order)

1. `./build/zpu-miner-<vendor> selftest tests/vectors/fp8c_v0.1.json --backend gpu --gemm blas-int8`: all 278 checks must PASS.
2. Repeat with `--gemm lt-fp8` and `--gemm naive`. Record FAIL as a result, not as an error to hide.
3. `python miner/python/zpu_miner.py stress --backend torch --device cuda --gemm int8|fp8|fp32` must show 0 mismatches. It forces |sum| = 2^20 rows that a random test would miss.
4. `bench/run_benchmarks.sh` records the throughput rows into `bench/results/miners.jsonl`.
5. Mine on the testnet: `zpu-cli mine --backend exec --exec "./build/zpu-miner-cuda --backend gpu --gemm blas-int8"`.

Sources for the B300 INT8 figure (third-party spec compilations; confirm against NVIDIA's datasheet before relying on exact values): [Tom's Hardware](https://www.tomshardware.com/pc-components/gpus/nvidia-shares-blackwell-ultras-secrets-nvfp4-boost-detailed-and-pcie-6-0-support), [DataCrunch](https://datacrunch.io/blog/nvidia-b300-vs-b200-complete-gpu-comparison-to-date), [flopper.io](https://flopper.io/compare/nvidia-b200-sxm-180gb-vs-nvidia-b300-sxm-288gb/spec-sheet), [Glenn Lockwood](https://glennklockwood.com/garden/processors/B300).
