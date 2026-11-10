# Hardware guide

The consensus function is defined on integers in {±1..±16}, with every accumulation below 2^24. Any device whose GEMM has an exact int32 or a true FP32 accumulator gets the canonical bits. The miner chooses the storage format per device. A wrong path cannot produce accepted blocks, because watchers (and, in the own-chain alternative, nodes) verify on the CPU. It can only waste the miner's work, and `zpu-cli` detects that (every winner plus random spot checks are re-verified on the CPU).

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

## Measured (2026-11-09)

Rerun 2 (`bench/results/gpu_rerun_2026-11-09_r2/`, ephemeral Cloud sandboxes, ledger `bench/results/gpu_ledger.json`). Every GEMM path passed the 278-check selftest and the 2^20 accumulator stress; every benchmark re-checked sample digests against the CPU reference.

| GPU | path | attempts/s | TMAC/s | launch overhead | board power | exact |
|---|---|---|---|---|---|---|
| H100 80GB HBM3 | `blas-int8` | 8,078,510 | 417 | 0.11 s | ~690 W | PASS |
| H100 | `lt-fp8` (E4M3) | 7,379,190 | 381 | 0.43 s | ~690 W | PASS |
| MI355X | `blas-int8` | 13,124,061 | 678 | 0.13 s | ~1,230 W | PASS |
| MI355X | `lt-fp8` (E4M3) | 13,102,900 | 677 | 1.25 s | ~1,200 W | PASS |

Real mining on the production v0.1 profile (`bench/gpu/box_mine.sh`): GPU miner -> `zpu-cli` -> CPU-only node, 120 s, then a second CPU-only node syncs and re-validates the chain from scratch.

| GPU | effective mining rate | blocks accepted | rejected | independent re-sync |
|---|---|---|---|---|
| H100 | 7.56 M attempts/s | 42 | 0 | same tip (height 42) |
| MI355X | 12.39 M attempts/s | 57 | 0 | same tip (height 57) |

Rerun 1 found that `zpu-cli` handed GPU miners 2,000-attempt jobs, capping real mining at 29k attempts/s; adaptive job sizing (`--job-secs`, default 0.5 s) fixed it. The earlier H100/MI355X measurements in `bench/results/gpu_summary.json` agree within 1%.

Not measured on hardware (no device reachable): B200/B300 (sm_100), RTX 50 / RTX PRO 6000 (sm_120), MI300X (gfx942, E4M3FNUZ). Device code for all of them compiles in `make check` (`gpu-device-compile`).

ROCm from pip (`_rocm_sdk_devel`, e.g. the `rocm/vllm` images) keeps rocBLAS/hipBLASLt code objects in `_rocm_sdk_libraries/lib/{rocblas,hipblaslt}/library`. Point `ROCBLAS_TENSILE_LIBPATH` / `HIPBLASLT_TENSILE_LIBPATH` there (`bench/gpu/box_run.sh` does it automatically), or hipBLAS/hipBLASLt fail with "Could not initialize Tensile host" / "hipModuleLoad failed".

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
