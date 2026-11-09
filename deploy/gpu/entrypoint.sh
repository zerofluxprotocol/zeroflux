#!/usr/bin/env bash
# zfx-gpu: container entrypoint for the GPU miner images.
#   gpu-test                 selftest (bit-exact vs vectors) + 2^20 accumulator
#                            stress + benchmark JSON for every GEMM path;
#                            exits non-zero on any failure
#   mine --node URL --address zfx1...   mine with the GPU backend via zpu-cli
#   <anything else>          exec as a command
set -uo pipefail
BIN=${ZPU_GPU_BIN:?}
V=/opt/zfx/vectors/fp8c_v0.1.json
if [ "$BIN" = zpu-miner-hip ]; then
  # Some ROCm images keep rocBLAS/hipBLASLt kernel objects outside /opt/rocm.
  RB=$(find / -path /proc -prune -o -name "TensileLibrary*gfx9*" -print 2>/dev/null | grep -m1 rocblas/library | xargs -r dirname)
  HB=$(find / -path /proc -prune -o -name "Kernels.so-000-gfx9*" -print 2>/dev/null | grep -m1 hipblaslt | xargs -r dirname)
  [ -n "$RB" ] && export ROCBLAS_TENSILE_LIBPATH=$RB
  [ -n "$HB" ] && export HIPBLASLT_TENSILE_LIBPATH=$HB
fi
case "${1:-gpu-test}" in
  gpu-test)
    fail=0
    $BIN selftest "$V" --backend cpu --threads 8 >/dev/null || { echo "FAIL cpu selftest"; fail=1; }
    for g in blas-int8 lt-fp8 naive; do
      if $BIN selftest "$V" --backend gpu --gemm $g | tail -1; then :; else echo "FAIL selftest $g"; fail=1; fi
      $BIN stress --backend gpu --gemm $g || { echo "FAIL stress $g"; fail=1; }
      $BIN bench --backend gpu --gemm $g --batch 32768 --count 131072 || { echo "FAIL bench $g"; fail=1; }
    done
    python3 /opt/zfx/python/zpu_miner.py selftest "$V" --backend torch --device cuda --gemm int8 | tail -1 || { echo "FAIL torch selftest"; fail=1; }
    [ $fail = 0 ] && echo "GPU-TEST PASS" || echo "GPU-TEST FAIL"
    exit $fail ;;
  mine)
    shift
    node="" addr=""
    while [ $# -gt 0 ]; do case $1 in --node) node=$2; shift 2;; --address) addr=$2; shift 2;; *) shift;; esac; done
    exec zpu-cli mine --node "${node:?--node required}" --address "${addr:?--address required}" --backend exec --exec "$BIN --backend gpu --gemm blas-int8" ;;
  *) exec "$@" ;;
esac
