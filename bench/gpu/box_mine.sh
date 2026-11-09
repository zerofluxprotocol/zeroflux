#!/usr/bin/env bash
# Real block submissions from the GPU miner (Gate 2). Runs inside the GPU box
# after box_run.sh has built the miner and the Rust binaries.
#   box_mine.sh "<miner command>" SECONDS
# Node A (CPU only) serves work for the production profile zfx-fp8c-v0.1 and
# fully validates every submitted block on the CPU. Node B (CPU only) then
# syncs the whole chain from A and must reach the same tip: an independent
# re-validation of every GPU-mined block. Prints one JSON line.
set -u
MINER=$1; SECS=${2:-120}; LOGS=${3:-}
T=$(mktemp -d)
B=./target/release
get() { python3 -c "import urllib.request,sys;print(urllib.request.urlopen(sys.argv[1],timeout=5).read().decode())" "$1" 2>/dev/null; }
GT=${GENESIS_TARGET:-000000ffffffffffffffffffffffffffffffffffffffffffffffffffffffffff}
$B/zfx-wallet new --wallet $T/miner.json >/dev/null
ADDR=$($B/zfx-wallet address --wallet $T/miner.json | grep -m1 -oE 'zfx1[0-9a-f]+')
# v0.1 profile, LWMA retarget; genesis target 2^232 (~16.7 M attempts/block) so a
# single GPU finds blocks every few seconds; genesis 1 h in the past.
$B/zfx-node print-config --testnet | python3 -c "
import json,sys,time
c=json.load(sys.stdin)
c.update(genesis_timestamp=int(time.time())-3600, genesis_target='$GT', target_spacing_secs=5, label='zfx-gpu-mining-check', allocations=[])
json.dump(c,open('$T/chain.json','w'))"
$B/zfx-node --config $T/chain.json --data-dir $T/a --p2p-port 19540 --rpc-port 19541 --no-discovery > $T/nodeA.log 2>&1 &
NA=$!
sleep 3
$B/zpu-cli mine --node http://127.0.0.1:19541 --address "$ADDR" --backend exec --exec "$MINER" > $T/miner.log 2>&1 &
NM=$!
sleep "$SECS"
kill $NM; wait $NM 2>/dev/null
STA=$(get http://127.0.0.1:19541/v1/status)
$B/zfx-node --config $T/chain.json --data-dir $T/b --p2p-port 19542 --rpc-port 19543 --peer 127.0.0.1:19540 --no-discovery > $T/nodeB.log 2>&1 &
NB=$!
for i in $(seq 1 120); do
  STB=$(get http://127.0.0.1:19543/v1/status)
  python3 -c "import json,sys;a=json.loads(sys.argv[1]);b=json.loads(sys.argv[2]);sys.exit(0 if a['best_hash']==b['best_hash'] else 1)" "$STA" "$STB" 2>/dev/null && break
  sleep 1
done
kill $NA $NB; wait 2>/dev/null
[ -n "$LOGS" ] && cp $T/miner.log $T/nodeA.log $T/nodeB.log $T/chain.json "$LOGS/" 2>/dev/null
python3 - "$STA" "$STB" "$T/miner.log" "$MINER" "$SECS" <<'EOF'
import json, re, sys
a, b = json.loads(sys.argv[1]), json.loads(sys.argv[2] or "{}")
log = open(sys.argv[3]).read()
acc = re.findall(r"block ([0-9a-f]{64}) accepted at height (\d+)", log)
last = [l for l in log.splitlines() if "rejected=" in l][-1:] or [""]
rej = re.search(r"rejected=(\d+)", last[0])
print(json.dumps({"step": "gpu_block_submissions", "profile": "zfx-fp8c-v0.1", "miner": sys.argv[4], "seconds": int(sys.argv[5]),
    "blocks_accepted_by_cpu_node": len(acc), "rejected": int(rej.group(1)) if rej else None, "first": acc[:2], "last": acc[-2:],
    "nodeA_tip": {k: a.get(k) for k in ("height", "best_hash")}, "nodeB_independent_sync_tip": {k: b.get(k) for k in ("height", "best_hash")},
    "independent_revalidation_matches": a.get("best_hash") == b.get("best_hash") and a.get("height", 0) > 0,
    "result": "PASS" if acc and a.get("best_hash") == b.get("best_hash") else "FAIL"}))
EOF
