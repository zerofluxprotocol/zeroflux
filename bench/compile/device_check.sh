#!/usr/bin/env bash
# Compile every ZPU GPU kernel for NVIDIA (sm_90 Hopper, sm_100 B200,
# sm_120 RTX 50/RTX PRO 6000) and AMD (gfx942 MI300X, gfx950 MI355X) device
# targets with a bare clang (zig's bundled clang >= 18 works), no vendor SDK.
# Output: bench/results/device_check.json
set -u
cd "$(dirname "$0")"
CLANG=${CLANG:-"python3 -m ziglang clang"}
OUT=../results/device_check
mkdir -p "$OUT"
results=()
for a in sm_90 sm_100 sm_120; do
  if $CLANG -x cuda -std=c++17 -O3 --cuda-gpu-arch=$a --cuda-device-only -nocudainc -nocudalib -isystem shim_include \
      -Xclang -target-feature -Xclang +ptx87 -S device_check.cu -o "$OUT/kernels_$a.ptx" 2> "$OUT/kernels_$a.err"; then
    n=$(grep -c "^.visible .entry" "$OUT/kernels_$a.ptx"); results+=("{\"target\":\"$a\",\"result\":\"PASS\",\"kernels\":$n}")
  else results+=("{\"target\":\"$a\",\"result\":\"FAIL\",\"error\":\"see $OUT/kernels_$a.err\"}"); fi
done
for a in gfx942 gfx950; do
  if $CLANG -x hip -std=c++17 -O3 --offload-arch=$a --cuda-device-only -nogpuinc -nogpulib -isystem shim_include \
      -S device_check.cu -o "$OUT/kernels_$a.s" 2> "$OUT/kernels_$a.err"; then
    n=$(grep -c "^\s*\.amdhsa_kernel " "$OUT/kernels_$a.s"); results+=("{\"target\":\"$a\",\"result\":\"PASS\",\"kernels\":$n}")
  else results+=("{\"target\":\"$a\",\"result\":\"FAIL\",\"error\":\"see $OUT/kernels_$a.err\"}"); fi
done
ver=$($CLANG --version | head -1)
(IFS=,; echo "{\"compiler\":\"$ver\",\"scope\":\"device code of all kernels incl. shared SHA-256/SHAKE256/requant; host BLAS calls not covered\",\"targets\":[${results[*]}]}") > ../results/device_check.json
cat ../results/device_check.json
