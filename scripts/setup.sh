#!/usr/bin/env bash
# One-time setup for running every test suite in this repository from a fresh clone.
# Idempotent: steps that are already done are skipped. About 10 to 15 minutes the first time.
#
#   scripts/setup.sh            everything below
#   scripts/setup.sh --no-wasm  skip the NEAR contract builds (unit tests still run)
#
# What each step enables:
#   python   .venv (Python 3.11+) for make check, the e2e scripts and the miners
#   sandbox  near-sandbox binary for the markets sandbox tests (scripts/install-near-sandbox.sh)
#   wasm     contract builds the sandbox tests deploy: markets/index oracle, zfx-perps contracts
#   node     zfx-perps-node binary, used by the TS SDK end-to-end test
#   js       pnpm install for the TS SDK, npm ci for the explorer typecheck
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
WASM=1
[ "${1:-}" = "--no-wasm" ] && WASM=0

say() { printf '\n== %s\n' "$*"; }
need() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1 ($2)" >&2; exit 1; }; }

say "tools"
need cargo "install Rust from https://rustup.rs"
need c++ "a C++17 compiler (Xcode command line tools or build-essential)"
need node "Node 20+"
need curl "curl"
echo "cargo $(cargo --version | cut -d' ' -f2), node $(node --version)"
# markets/zfx-perps pins its own toolchain (rust-toolchain.toml); rustup fetches it on first use.

say "python (.venv)"
if [ ! -x .venv/bin/python ]; then
  if command -v uv >/dev/null 2>&1; then
    uv venv -q --python 3.12 .venv
  else
    PYBIN=$(for p in python3.13 python3.12 python3.11; do command -v $p && break; done || true)
    [ -n "$PYBIN" ] || { echo "need Python 3.11+ or uv (https://docs.astral.sh/uv/)" >&2; exit 1; }
    "$PYBIN" -m venv .venv
  fi
fi
if command -v uv >/dev/null 2>&1; then
  uv pip install -q --python .venv/bin/python numpy jsonschema websockets ziglang
else
  .venv/bin/python -m pip install -q numpy jsonschema websockets ziglang
fi
.venv/bin/python --version

say "near-sandbox"
scripts/install-near-sandbox.sh

if [ "$WASM" = 1 ]; then
  say "cargo-near and NEAR contract builds"
  if ! cargo near --version >/dev/null 2>&1; then
    cargo install --locked cargo-near
  fi
  (cd markets/index/contracts/zfx-oracle && cargo near build non-reproducible-wasm --no-abi >/dev/null)
  echo "markets/index oracle wasm built"
  markets/zfx-perps/scripts/build-contracts.sh
fi

say "zfx-perps node binary"
(cd markets/zfx-perps && cargo build --release -q -p zfx-perps-node)

say "JavaScript dependencies"
if ! command -v pnpm >/dev/null 2>&1; then
  command -v corepack >/dev/null 2>&1 && corepack enable >/dev/null 2>&1 || true
fi
need pnpm "corepack enable, or npm i -g pnpm"
(cd markets/zfx-perps/packages/sdk-ts && pnpm install --frozen-lockfile --silent)
(cd explorer && npm ci --no-audit --no-fund --silent)

say "done"
echo "Next: the commands in AGENTS.md ('Claims and how to verify them')."
