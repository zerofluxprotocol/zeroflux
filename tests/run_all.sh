#!/usr/bin/env bash
# Run every ZeroFlux test suite on this host and print PASS/FAIL per suite.
# Usage: tests/run_all.sh [--with-testnet]
set -u
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
PY=${PY:-.venv/bin/python}
fail=0
run() { local name="$1"; shift; if "$@" > "/tmp/zfx-test-$name.log" 2>&1; then echo "PASS $name"; else echo "FAIL $name (see /tmp/zfx-test-$name.log)"; fail=1; fi; }
run cargo-test cargo test --release -q
run cargo-build cargo build --release -q
run vectors-rust ./target/release/zpu-cli selftest
run cpp-build make -C miner/cuda cpu -s
run vectors-cpp ./miner/cuda/build/zpu-miner selftest tests/vectors/fp8c_v0.1.json
run vectors-python-numpy $PY miner/python/zpu_miner.py selftest tests/vectors/fp8c_v0.1.json --backend numpy
run stress-python-numpy $PY miner/python/zpu_miner.py stress --backend numpy
if $PY -c "import torch; assert torch.backends.mps.is_available()" 2>/dev/null; then
  run vectors-torch-mps $PY miner/python/zpu_miner.py selftest tests/vectors/fp8c_v0.1.json --backend torch --device mps
  run stress-torch-mps $PY miner/python/zpu_miner.py stress --backend torch --device mps
fi
if $PY -c "import mlx.core" 2>/dev/null; then
  run vectors-mlx $PY miner/python/zpu_miner.py selftest tests/vectors/fp8c_v0.1.json --backend mlx
  run stress-mlx $PY miner/python/zpu_miner.py stress --backend mlx
fi
if $PY -c "import torch; assert torch.cuda.is_available()" 2>/dev/null; then
  for g in fp32 int8 fp8; do
    run vectors-torch-cuda-$g $PY miner/python/zpu_miner.py selftest tests/vectors/fp8c_v0.1.json --backend torch --device cuda --gemm $g
    run stress-torch-cuda-$g $PY miner/python/zpu_miner.py stress --backend torch --device cuda --gemm $g
  done
fi
if $PY -c "import ziglang" 2>/dev/null || [ -n "${CLANG:-}" ]; then
  run gpu-device-compile bench/compile/device_check.sh
fi
run explorer-typecheck bash -c "cd explorer && npx --no-install tsc -p . --noEmit"
if [ "${1:-}" = "--with-testnet" ]; then
  run testnet-dev python3 testnet/zfxnet.py scenario --profile dev
fi
exit $fail
