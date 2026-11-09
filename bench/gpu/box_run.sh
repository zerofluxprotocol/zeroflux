#!/usr/bin/env bash
# Runs inside an ephemeral GPU sandbox. Usage: box_run.sh cuda|hip ARCH
# Writes $W/results/{results.jsonl,*.log}. Every step records PASS/FAIL; nothing is skipped silently.
set -u
V=$1; ARCH=$2
W=${W:-$HOME/zfxwork}; cd "$W/zeroflux"
R=$W/results; mkdir -p "$R"
J=$R/results.jsonl
jesc() { python3 -c 'import json,sys;print(json.dumps(sys.stdin.read().strip()[-1500:]))'; }
rec() { echo "$1" >> "$J"; echo "$1"; }
step() { # name, command...
  local n=$1; shift
  local t0=$(date +%s.%N)
  "$@" > "$R/$n.log" 2>&1; local rc=$?
  local el=$(python3 -c "print(round($(date +%s.%N)-$t0,2))")
  rec "{\"step\":\"$n\",\"rc\":$rc,\"result\":\"$([ $rc = 0 ] && echo PASS || echo FAIL)\",\"elapsed_s\":$el,\"tail\":$(tail -3 "$R/$n.log" | jesc)}"
  return $rc
}
power_start() { # sample board power every 200 ms into $R/power_$1.csv
  if [ "$V" = cuda ]; then nvidia-smi --query-gpu=power.draw --format=csv,noheader,nounits -lms 200 > "$R/power_$1.csv" 2>/dev/null &
  else ( while true; do
      w=$(cat /sys/class/drm/card*/device/hwmon/hwmon*/power1_average 2>/dev/null | awk '$1>0{print $1/1e6; exit}')
      [ -z "$w" ] && w=$(amd-smi metric -p 2>/dev/null | grep -iE "socket_power|power_usage|current_socket" | grep -oE "[0-9]+(\.[0-9]+)?" | head -1)
      [ -n "$w" ] && echo "$w" >> "$R/power_$1.csv"
      sleep 0.2; done ) & fi
  PPID_SAMPLER=$!
}
power_stop() { kill $PPID_SAMPLER 2>/dev/null; wait $PPID_SAMPLER 2>/dev/null; }

(amd-smi metric -p 2>&1 | head -20; ls /sys/class/drm/card*/device/hwmon/hwmon*/ 2>&1 | head -20) > "$R/power_probe.txt" 2>&1
rec "{\"step\":\"host\",\"uname\":\"$(uname -a)\",\"cpus\":$(nproc)}"
if [ "$V" = cuda ]; then step gpu_info nvidia-smi; step toolchain nvcc --version
else step gpu_info bash -c "(amd-smi static --asic 2>/dev/null || rocm-smi --showproductname); rocminfo | grep -m4 -E 'gfx|Marketing'"; step toolchain bash -c "hipcc --version || /opt/rocm/bin/hipcc --version"; fi
command -v make >/dev/null || step apt_make bash -c "apt-get update -qq && apt-get install -y -qq make g++ >/dev/null"

if [ "$V" = cuda ]; then step build make -C miner/cuda cuda CUDA_ARCH="$ARCH"
else step build make -C miner/cuda hip HIPCC="$(command -v hipcc || echo /opt/rocm/bin/hipcc)" HIP_ARCH="$ARCH"; fi
BIN=miner/cuda/build/zpu-miner-$V
if [ "$V" = hip ]; then
  # pip-packaged ROCm SDKs may ship rocBLAS/hipBLASLt kernel libraries outside
  # the SDK dir. Find the directories that really hold gfx950 code objects and
  # point only the C++ miner at them (PyTorch keeps its own defaults).
  find / \( -name "Kernels.so-000-gfx950*" -o -name "TensileLibrary*gfx950*" \) 2>/dev/null | head -40 > "$R/rocm_kernel_files.txt"
  RB=$(grep -m1 "rocblas/library/TensileLibrary" "$R/rocm_kernel_files.txt" | xargs -r dirname)
  HB=$(grep -m1 "hipblaslt.*Kernels.so-000-gfx950" "$R/rocm_kernel_files.txt" | xargs -r dirname)
  CPPENV="env ${RB:+ROCBLAS_TENSILE_LIBPATH=$RB} ${HB:+HIPBLASLT_TENSILE_LIBPATH=$HB}"
  rec "{\"step\":\"rocm_libpaths\",\"rocblas\":\"$RB\",\"hipblaslt\":\"$HB\",\"HSA_XNACK\":\"${HSA_XNACK:-unset}\"}"
else
  CPPENV=env
fi
BIN="$CPPENV $BIN"
step cpu_selftest_in_gpu_binary $BIN selftest tests/vectors/fp8c_v0.1.json --backend cpu --threads 8

for g in blas-int8 lt-fp8 naive; do
  step "selftest_$g" $BIN selftest tests/vectors/fp8c_v0.1.json --backend gpu --gemm $g
  step "stress_$g" $BIN stress --backend gpu --gemm $g
  # warm run, then size the timed run to about 10 s so power samples are meaningful
  rate=$($BIN bench --backend gpu --gemm $g --batch 32768 --count 131072 2>/dev/null | python3 -c "import json,sys;print(int(json.loads(sys.stdin.read().strip().splitlines()[-1])['attempts_per_s']))" 2>/dev/null || echo 0)
  cnt=$(( (rate * 10 / 32768 + 1) * 32768 )); [ $cnt -lt 131072 ] && cnt=131072
  for pb in 8192 32768; do step "profile_${g}_b$pb" $BIN profile --backend gpu --gemm $g --batch $pb; done
  power_start $g
  step "bench_$g" $BIN bench --backend gpu --gemm $g --batch 32768 --count $cnt
  power_stop
  bj=$(grep -h '^{' "$R/bench_$g.log" | tail -1); bj=${bj:-null}
  pw=$(python3 bench/gpu/avgpower.py "$R/power_$g.csv" 2>/dev/null || echo null)
  rec "{\"step\":\"bench_json_$g\",\"data\":$bj,\"avg_power_w\":$pw,\"power_samples\":$(wc -l < "$R/power_$g.csv" 2>/dev/null || echo 0)}"
done

# Third implementation on the vendor's PyTorch build (CUDA or ROCm).
PY=python3
step torch_info $PY -c "import torch;print(torch.__version__, torch.version.cuda, torch.version.hip, torch.cuda.get_device_name(0))"
for g in fp32 int8 fp8; do
  step "py_selftest_$g" $PY miner/python/zpu_miner.py selftest tests/vectors/fp8c_v0.1.json --backend torch --device cuda --gemm $g
  step "py_stress_$g" $PY miner/python/zpu_miner.py stress --backend torch --device cuda --gemm $g
done
step py_bench_int8 $PY miner/python/zpu_miner.py bench --backend torch --device cuda --gemm int8 --count 65536 --batch 8192

# Linux x86-64 CPU: full Rust test suite + Rust reference vs vectors (needs network for crates).
if [ "${WITH_RUST:-0}" = 1 ]; then
  step rustup bash -c "python3 -c \"import urllib.request as u;u.urlretrieve('https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init','/tmp/rustup-init')\" && chmod +x /tmp/rustup-init && /tmp/rustup-init -y --profile minimal"
  export PATH=$HOME/.cargo/bin:$PATH
  step cargo_test cargo test --release -q
  step rust_selftest bash -c "cargo build --release -q && ./target/release/zpu-cli selftest"
  step cpp_cpu_selftest_x86 bash -c "make -C miner/cuda cpu -s && ./miner/cuda/build/zpu-miner selftest tests/vectors/fp8c_v0.1.json"
  step rust_bench_x86 ./target/release/zpu-cli bench --profile v01 --seconds 5
fi
echo DONE
