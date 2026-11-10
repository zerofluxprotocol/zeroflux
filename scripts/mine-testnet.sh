#!/usr/bin/env bash
# Mine ZFX on NEAR testnet against the public contract zfx.zeroflux.testnet.
#
#   scripts/mine-testnet.sh <account>.testnet [cpu|mlx|mps|cuda|hip]
#
# Needs: a NEAR testnet account whose key is in ~/.near-credentials/testnet/
# (create one free with the faucet, see below) holding about 4 NEAR for bonds
# (0.25 NEAR per pending block, returned when its 10-minute challenge window
# passes; a fast miner can have 10 or more blocks pending at once), and a built
# checkout (`cargo build --release`, plus `make -C miner/cuda cuda|hip` for GPUs
# or the Python env for mlx/mps).
#
# Free account from the official testnet faucet (NEAR CLI, `cargo install near-cli-rs`):
#   near account create-account sponsor-by-faucet-service <you>.testnet \
#     autogenerate-new-keypair save-to-legacy-keychain network-config testnet create
#
# Runs, until Ctrl-C:
#   - a mining proxy that turns the miner's work requests into NEAR calls
#     (it attaches the contract's bond to every block it submits),
#   - a watcher that re-checks every pending block and challenges fraud
#     (catching a cheater pays you their bond),
#   - a finalizer that mints rewards for blocks whose challenge window passed,
#   - the miner on the backend you chose.
set -euo pipefail

ACCOUNT="${1:-}"
BACKEND="${2:-cpu}"
CONTRACT="${ZFX_CONTRACT:-zfx.zeroflux.testnet}"
RPC="${NEAR_TESTNET_RPC:-https://rpc.testnet.fastnear.com,https://test.rpc.fastnear.com,https://rpc.testnet.near.org}"
PORT="${ZFX_PROXY_PORT:-9651}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release"
KEY="${NEAR_KEY_FILE:-$HOME/.near-credentials/testnet/$ACCOUNT.json}"

die() { echo "mine-testnet: $*" >&2; exit 1; }
[[ "$ACCOUNT" == *.testnet ]] || die "usage: scripts/mine-testnet.sh <account>.testnet [cpu|mlx|mps|cuda|hip]"
[[ -f "$KEY" ]] || die "no key for $ACCOUNT at $KEY (create the account with the faucet command in this script's header)"
for b in zfx-bridge zfx-miner; do [[ -x "$BIN/$b" ]] || die "missing $BIN/$b: run cargo build --release"; done

case "$BACKEND" in
  cpu) MINER=(--threads "$(getconf _NPROCESSORS_ONLN)") ;;
  mlx) [[ -x "$ROOT/.venv/bin/python" ]] || die "no .venv: see README prerequisites"
       MINER=(--backend exec --exec "$ROOT/.venv/bin/python $ROOT/miner/python/zpu_miner.py --backend mlx") ;;
  mps) [[ -x "$ROOT/.venv/bin/python" ]] || die "no .venv: see README prerequisites"
       MINER=(--backend exec --exec "$ROOT/.venv/bin/python $ROOT/miner/python/zpu_miner.py --backend torch --device mps") ;;
  cuda|hip) [[ -x "$ROOT/miner/cuda/build/zpu-miner-$BACKEND" ]] || die "build the GPU miner: make -C miner/cuda $BACKEND"
            MINER=(--backend exec --exec "$ROOT/miner/cuda/build/zpu-miner-$BACKEND --backend gpu --gemm blas-int8") ;;
  *) die "unknown backend $BACKEND (cpu, mlx, mps, cuda, hip)" ;;
esac

view() { "$BIN/zfx-bridge" view --rpc "$RPC" --contract "$CONTRACT" --method "$1" --args "$2" | tail -1; }
call() { "$BIN/zfx-bridge" call --rpc "$RPC" --signer "$ACCOUNT" --key-file "$KEY" --contract "$CONTRACT" "$@" | tail -1; }

echo "contract  $CONTRACT"
echo "stats     $(view stats '{}')"
if [[ "$(view storage_balance_of "{\"account_id\":\"$ACCOUNT\"}")" == "null" ]]; then
  echo "registering $ACCOUNT with the ZFX token (0.00125 NEAR storage deposit)"
  call --method storage_deposit --args '{"registration_only":true}' --deposit 1250000000000000000000 --gas 30 >/dev/null
fi

LOG="${ZFX_LOG_DIR:-$ROOT/deploy/run/testnet-miner}"
mkdir -p "$LOG"
pids=()
cleanup() {
  trap - EXIT INT TERM
  kill "${pids[@]}" 2>/dev/null || true
  echo; echo "balance   $(view ft_balance_of "{\"account_id\":\"$ACCOUNT\"}") (ZFX base units, 8 decimals)"
  exit 0
}
trap cleanup EXIT INT TERM

"$BIN/zfx-bridge" near-opt-watch --interval-ms 3000 --rpc "$RPC" --signer "$ACCOUNT" --key-file "$KEY" --contract "$CONTRACT" >"$LOG/watch.log" 2>&1 &
pids+=($!)
"$BIN/zfx-bridge" near-mine-proxy --optimistic --rpc "$RPC" --signer "$ACCOUNT" --key-file "$KEY" --contract "$CONTRACT" --listen "127.0.0.1:$PORT" >"$LOG/proxy.log" 2>&1 &
pids+=($!)
( while sleep 120; do call --method finalize --args '{"max":20}' --gas 100 >>"$LOG/finalize.log" 2>&1 || true; done ) &
pids+=($!)
sleep 2
echo "mining    backend=$BACKEND  logs in $LOG  (Ctrl-C to stop)"
# In the background plus `wait`, so a signal to this script (not only Ctrl-C in its
# terminal) runs cleanup and stops every child.
"$BIN/zfx-miner" mine --node "http://127.0.0.1:$PORT" "${MINER[@]}" &
pids+=($!)
wait "$!"
