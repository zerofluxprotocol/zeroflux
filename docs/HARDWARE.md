# Hardware guide

The consensus function is defined on integers in {±1..±16}, with every accumulation below 2^24. Any device whose GEMM has an exact int32 or a true FP32 accumulator gets the canonical bits. The miner chooses the storage format per device. A wrong path cannot produce accepted blocks, because watchers (and, in the own-chain alternative, nodes) verify on the CPU. It can only waste the miner's work, and `zpu-cli` detects that (every winner plus random spot checks are re-verified on the CPU).

## Which path per GPU

| GPU | arch | recommended path | alternative | FP8 encoding | notes |
|---|---|---|---|---|---|
| H100 / H200 | sm_90 | `--gemm blas-int8` (cuBLAS int8 -> int32, exact) | `lt-fp8` | OCP E4M3 | Hopper FP8 MMA accumulation precision is the known hazard; run `selftest` + `stress` before trusting `lt-fp8` |
| B200 | sm_100 | `blas-int8` | `lt-fp8` | OCP E4M3 | measure both |
| B300 (Blackwell Ultra) | sm_103 | `lt-fp8` | `blas-int8` (slow) | OCP E4M3 | dense INT8 is cut (traded for NVFP4); FP8 is exact under `stress` (measured 2026-11-11: 14.9 M attempts/s, the fastest device) |
| RTX 4090 / 5090, RTX PRO 6000 | sm_89 / sm_120 | `blas-int8` | `lt-fp8` | OCP E4M3 | consumer FP8 MMA with FP16 accumulate is inexact (26% of rows diverge in our model); use FP32-output paths only |
| MI300X | gfx942 | `blas-int8` (hipBLAS) | `lt-fp8` (hipBLASLt) | **E4M3FNUZ** (bias 8, no -0) | the miner selects FNUZ from `gcnArchName` |
| MI355X | gfx950 | `blas-int8` | `lt-fp8` | OCP E4M3 | |
| Apple M-series | Metal | `zpu_miner.py --backend mlx` | `--backend torch --device mps` | fp32 GEMM | measured bit-exact |
| any CPU | | `zpu-cli --backend cpu` or `zpu-miner --backend cpu` | NumPy fp32 BLAS | | reference |

## Measured (2026-11-11): Hopper, Blackwell, RTX and AMD

Round 6 (`bench/results/gpu-r6/`, summary `bench/results/gpu-r6/summary.json`). Six ephemeral cloud GPU sandboxes: five NVIDIA on one image (`pytorch/pytorch:2.12.0-cuda13.0-cudnn9-devel`) and an MI355X on ROCm 10 (`rocm/vllm:rocm10.0.0_ubuntu24.04_py3.14_pytorch_2.12.0_vllm_0.27.0`), each built natively for its architecture, each deleted afterwards and confirmed absent (ledger `bench/results/gpu_ledger.json`). On every device, every path passed the 278-check selftest and the 2^20 accumulator stress, and the PyTorch implementation passed selftest and stress in fp32, int8 and fp8: 27 of 27 steps on each NVIDIA device and 34 of 34 on the MI355X, which also ran the Rust workspace tests and the Rust and C++ CPU references on its Linux x86 host. No failures.

| GPU | path | attempts/s | TMAC/s | board power | µJ/attempt | exact |
|---|---|---|---|---|---|---|
| B300 SXM6 (Blackwell Ultra, sm_103) | `blas-int8` | 1,376,540 | 71 | ~438 W | 317.8 | PASS |
| B300 SXM6 (Blackwell Ultra, sm_103) | `lt-fp8` | 14,910,000 | 770 | ~1054 W | 70.7 | PASS |
| AMD Instinct MI355X (gfx950) | `blas-int8` | 13,043,963 | 674 | ~1247 W | 95.6 | PASS |
| AMD Instinct MI355X (gfx950) | `lt-fp8` | 13,065,200 | 675 | ~1211 W | 92.7 | PASS |
| H200 | `blas-int8` | 8,372,340 | 432 | ~690 W | 82.4 | PASS |
| H200 | `lt-fp8` | 7,627,060 | 394 | ~692 W | 90.7 | PASS |
| H100 80GB HBM3 | `blas-int8` | 7,882,540 | 407 | ~693 W | 87.9 | PASS |
| H100 80GB HBM3 | `lt-fp8` | 7,256,520 | 375 | ~693 W | 95.5 | PASS |
| RTX PRO 6000 Blackwell (sm_120) | `blas-int8` | 4,780,250 | 247 | ~596 W | 124.7 | PASS |
| RTX PRO 6000 Blackwell (sm_120) | `lt-fp8` | 4,852,340 | 251 | ~593 W | 122.2 | PASS |
| GeForce RTX 4090 (sm_89) | `blas-int8` | 3,286,270 | 170 | ~442 W | 134.7 | PASS |
| GeForce RTX 4090 (sm_89) | `lt-fp8` | 2,100,070 | 108 | ~450 W | 214.4 | PASS |

- **B300 mines on FP8.** Its dense INT8 is cut (1.38 M/s), but `lt-fp8` is exact under stress and the fastest path measured anywhere: 14.9 M attempts/s, 71 µJ per attempt, the best energy per attempt of any device. Run it with `--gemm lt-fp8`.
- **AMD:** the MI355X reproduces its 2026-11-09 numbers on the current code, 13.0 M/s on `blas-int8` and 13.1 M/s on `lt-fp8`, exact on both and in PyTorch-ROCm.
- **Hopper:** H200 is about 6% faster than H100 at the same power; `blas-int8` is the faster path on both.
- **RTX:** the RTX PRO 6000 Blackwell is exact on both paths at about 4.8 M/s; the RTX 4090 reaches 3.3 M/s on `blas-int8`. FP8 with FP32 output is exact on these consumer and workstation parts (the FP16-accumulate hazard in the table above does not apply to the shipped path).
- Not reached: B200 and RTX 5090 (no capacity offered on 2026-11-11) and MI300X (not offered by the cloud provider).

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
