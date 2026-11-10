#!/usr/bin/env bash
# One-command local exchange: nearcore sandbox + settlement contract + node + adapter + a market
# maker, plus two funded dev users (alice, bob). Ctrl-C stops everything.
#
#   scripts/dev.sh            # state in data/dev (wiped on start)
#
# Needs: Rust toolchain, cargo-near, a near-sandbox binary (NEAR_SANDBOX_BIN or
# ~/.local/share/basis-amm/near-sandbox).
set -euo pipefail
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
cd "$(dirname "$0")/.."
ROOT=$PWD
DEV=$ROOT/data/dev
PORT=${OBKIT_PORT:-8794}
TOKEN=dev-admin-token-change-me
rm -rf "$DEV" && mkdir -p "$DEV"
if lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
  echo "port $PORT is in use; set OBKIT_PORT" >&2; exit 1
fi
# Wait until a command succeeds, failing fast if the given process died.
wait_for() { local pid=$1 log=$2; shift 2; until "$@" >/dev/null 2>&1; do
  kill -0 "$pid" 2>/dev/null || { echo "process exited; see $log" >&2; tail -5 "$log" >&2; exit 1; }; sleep 0.3; done; }

echo "1/6 build contracts and binaries"
[ -f out/obkit_settlement.wasm ] || ./scripts/build-contracts.sh >/dev/null
cargo build --release -q -p obkit-node -p obkit-near
cargo build --release -q -p obkit-client --examples
BIN=$ROOT/target/release

pids=()
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true; }
trap cleanup EXIT INT TERM

echo "2/6 start sandbox and deploy (tokens, settlement contract, operator, fee account)"
"$BIN/obkit-near" sandbox --out "$DEV/stack.json" --force-timeout-secs 3600 2>"$DEV/sandbox.log" &
pids+=($!)
wait_for "${pids[0]}" "$DEV/sandbox.log" test -s "$DEV/stack.json"

echo "3/6 start node on :$PORT"
"$BIN/obkit-near" node-config --stack "$DEV/stack.json" --data "$DEV/node" --listen "127.0.0.1:$PORT" --admin-token "$TOKEN" >"$DEV/node.json"
"$BIN/obkit-node" --config "$DEV/node.json" 2>"$DEV/node.log" &
pids+=($!)
wait_for "${pids[1]}" "$DEV/node.log" curl -sf "http://127.0.0.1:$PORT/v1/health"

echo "4/6 start settlement adapter"
"$BIN/obkit-near" adapter --stack "$DEV/stack.json" --node "http://127.0.0.1:$PORT" --admin-token "$TOKEN" --poll-ms 300 2>"$DEV/adapter.log" &
pids+=($!)

echo "5/6 create dev users (mm, pmm, alice, bob) and fund the insurance fund"
"$BIN/obkit-near" fund-insurance --stack "$DEV/stack.json" --usdc 100000 >>"$DEV/users.log" 2>&1
for u in mm pmm alice bob; do
  "$BIN/obkit-near" dev-user --stack "$DEV/stack.json" --name "$u" --near 100000 --usdc 1000000 >"$DEV/$u.json" 2>>"$DEV/users.log"
done
acct() { python3 -c "import json,sys;print(json.load(open('$DEV/$1.json'))['$2'])"; }
wait_for "${pids[2]}" "$DEV/adapter.log" sh -c "curl -sf -H 'Authorization: Bearer $TOKEN' http://127.0.0.1:$PORT/internal/status | grep -q '\"accounts\":[6-9]'"

echo "6/6 start market makers (NEAR-USDC spot, NEAR-PERP around the oracle mark)"
OBKIT_URL="http://127.0.0.1:$PORT" OBKIT_ACCOUNT=$(acct mm account) OBKIT_TRADING_KEY=$(acct mm trading_key) \
  "$BIN/examples/market_maker" >"$DEV/mm.log" 2>&1 &
pids+=($!)
OBKIT_URL="http://127.0.0.1:$PORT" OBKIT_ACCOUNT=$(acct pmm account) OBKIT_TRADING_KEY=$(acct pmm trading_key) OBKIT_MARKET=NEAR-PERP \
  "$BIN/examples/market_maker" >"$DEV/pmm.log" 2>&1 &
pids+=($!)

cat <<MSG

obkit dev exchange running
  API        http://127.0.0.1:$PORT   (GET /v1/info, /v1/book/NEAR-USDC, /v1/perp/NEAR-PERP, WS /v1/ws)
  markets    NEAR-USDC (spot), NEAR-PERP (10x, mark = median of exchange tickers; --manual-marks for offline)
  chain      $(python3 -c "import json;print(json.load(open('$DEV/stack.json'))['rpc_url'])")   contract $(python3 -c "import json;print(json.load(open('$DEV/stack.json'))['contract'])")
  users      $DEV/{alice,bob,mm,pmm}.json  (account key + trading key)
  logs       $DEV/*.log

Take liquidity as alice:
  OBKIT_URL=http://127.0.0.1:$PORT OBKIT_ACCOUNT=$(acct alice account) OBKIT_TRADING_KEY=$(acct alice trading_key) $BIN/examples/taker buy 50

Ctrl-C to stop.
MSG
wait
