#!/bin/sh
# Re-capture the real terminal output used in the demo videos (demo/src/captures.ts).
# Requires: a built binary, a daemon with GLiNER loaded, and a mock upstream as the
# default destination. Synthetic values only.
#   PV_HOME=/tmp/pv-demo PV_KEY_BACKEND=file PV_PORT=8798 sh scripts/demo_capture.sh
set -eu
PV=${PROMPTVPN_BIN:-./target/debug/promptvpn}
"$PV" mock-upstream --listen 9796 --record /tmp/demo-upstream.jsonl >/dev/null 2>&1 &
mock=$!
trap 'kill $mock' EXIT
"$PV" init --non-interactive --model base --near-skip --custom-url http://127.0.0.1:9796/v1 --custom-model mock-echo --custom-ack >/dev/null
"$PV" entity add --label "insurance policy number"
"$PV" entity add --label "legal case number"
KEY=$(cat "$PV_HOME/apps/default.key")
mkdir -p /tmp/demo-cap
req() { curl -s "http://127.0.0.1:$PV_PORT/v1/chat/completions" -H "Authorization: Bearer $KEY" -H 'content-type: application/json' \
  -d "$(python3 -I -c 'import json,sys; print(json.dumps({"model":"gpt-5","messages":[{"role":"user","content":sys.argv[1]}]}))' "$1")"; }
req "Update payroll for my account. My SSN is 219-09-9999." > /tmp/demo-cap/ssn.json
req "Deploy with OPENAI_API_KEY=sk-proj-Q8zR3mPx9Tv2Wn7Ks4Yb6HdQ8zR3mPx9Tv2Wn7Ks" > /tmp/demo-cap/key.json
req "Draft a reply to maria.gonzalez@example.com about her refund." > /tmp/demo-cap/email.json
req "remind maria gonzalez her insurance policy number is HX-2291-0042, she has been managing her insulin dose" > /tmp/demo-cap/entity.json
req "Summarize the filing for legal case number 2024-CV-01187 before the hearing" > /tmp/demo-cap/case.json
"$PV" near verify-attestation > /tmp/demo-cap/near.txt
"$PV" near verify-attestation deepseek-ai/DeepSeek-V4-Flash > /tmp/demo-cap/near-alias.txt || true
echo "captured into /tmp/demo-cap and /tmp/demo-upstream.jsonl"
