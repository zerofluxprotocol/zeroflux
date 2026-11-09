#!/usr/bin/env bash
# Reproduce every number in docs/BENCHMARKS.md on the current host.
# Writes bench/results/miners.jsonl (one JSON object per line).
# GPU backends that this host lacks are recorded as UNSUPPORTED, not skipped.
set -u
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
PY=${PY:-.venv/bin/python}
OUT=bench/results/miners.jsonl
: > "$OUT"
rec() { echo "$1" | tee -a "$OUT"; }
unsupported() { rec "{\"backend\":\"$1\",\"result\":\"UNSUPPORTED\",\"reason\":\"$2\"}"; }
host=$( (sysctl -n machdep.cpu.brand_string 2>/dev/null || grep -m1 'model name' /proc/cpuinfo | cut -d: -f2) | xargs)
rec "{\"host\":\"$host\",\"threads\":$(getconf _NPROCESSORS_ONLN),\"date\":\"$(date -u +%FT%TZ)\"}"

cargo build --release -q
make -C miner/cuda cpu -s

# Correctness first: every implementation against the published vectors.
for be in "./target/release/zpu-cli selftest" "./miner/cuda/build/zpu-miner selftest tests/vectors/fp8c_v0.1.json"; do
  r=$($be | tail -1); rec "{\"selftest\":\"$be\",\"summary\":\"$r\"}"
done
for be in "--backend numpy" "--backend torch --device cpu --gemm fp32" "--backend torch --device mps --gemm fp32" "--backend mlx"; do
  r=$($PY miner/python/zpu_miner.py selftest tests/vectors/fp8c_v0.1.json $be 2>&1 | tail -1); rec "{\"selftest\":\"python $be\",\"summary\":\"$r\"}"
  s=$($PY miner/python/zpu_miner.py stress $be 2>&1 | tail -1); rec "$s"
done

# Throughput, v0.1 production profile and dev profile.
T=$(getconf _NPROCESSORS_ONLN)
for prof in v01 dev; do
  for th in 1 $T; do
    rec "$(./target/release/zpu-cli bench --profile $prof --threads $th --seconds 5)"
    c=$([ $prof = v01 ] && echo $((300*th)) || echo $((20000*th)))
    rec "$(./miner/cuda/build/zpu-miner bench --profile $prof --threads $th --count $c)"
  done
done
for be in "--backend numpy" "--backend mlx" "--backend torch --device mps --gemm fp32"; do
  rec "$($PY miner/python/zpu_miner.py bench --profile v01 --count 16384 --batch 2048 $be 2>&1 | tail -1)"
done

# Vendor GPU backends.
if command -v nvidia-smi >/dev/null 2>&1; then
  make -C miner/cuda cuda -s
  for g in blas-int8 lt-fp8 naive; do
    rec "$(./miner/cuda/build/zpu-miner-cuda selftest tests/vectors/fp8c_v0.1.json --backend gpu --gemm $g | tail -1)"
    rec "$(./miner/cuda/build/zpu-miner-cuda bench --backend gpu --gemm $g --batch 8192 --count 262144)"
  done
  for g in int8 fp8 fp32; do rec "$($PY miner/python/zpu_miner.py stress --backend torch --device cuda --gemm $g | tail -1)"; done
else
  unsupported "cuda (H100/H200/B200/RTX)" "no NVIDIA GPU on this host"
fi
if command -v rocm-smi >/dev/null 2>&1; then
  make -C miner/cuda hip -s
  for g in blas-int8 lt-fp8 naive; do
    rec "$(./miner/cuda/build/zpu-miner-hip selftest tests/vectors/fp8c_v0.1.json --backend gpu --gemm $g | tail -1)"
    rec "$(./miner/cuda/build/zpu-miner-hip bench --backend gpu --gemm $g --batch 8192 --count 262144)"
  done
else
  unsupported "rocm (MI300X/MI355X)" "no AMD GPU on this host"
fi
echo "wrote $OUT"
