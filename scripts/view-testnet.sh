#!/usr/bin/env bash
# Read a ZeroFlux contract on NEAR testnet with nothing but curl and python3 (no build, no keys).
#   scripts/view-testnet.sh zfx.zeroflux.testnet stats '{}'
#   scripts/view-testnet.sh oracle.zeroflux.testnet latest '{"index_id":"or-model-spend-lic-7d/anthropic/claude-opus-5.5-20260921"}'
#   scripts/view-testnet.sh perps-v2.zeroflux.testnet get_markets '{}'
set -euo pipefail
CONTRACT="$1"; METHOD="$2"; ARGS="${3:-{\}}"
RPC="${NEAR_TESTNET_RPC:-https://rpc.testnet.near.org}"
B64=$(printf '%s' "$ARGS" | base64 | tr -d '\n')
curl -s "$RPC" -H 'content-type: application/json' -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"query\",\"params\":{\"request_type\":\"call_function\",\"finality\":\"final\",\"account_id\":\"$CONTRACT\",\"method_name\":\"$METHOD\",\"args_base64\":\"$B64\"}}" \
  | python3 -c 'import sys,json; r=json.load(sys.stdin); res=r.get("result",{}); print(json.dumps(json.loads(bytes(res["result"]).decode()), indent=1) if "result" in res else json.dumps(r.get("error") or res, indent=1))'
