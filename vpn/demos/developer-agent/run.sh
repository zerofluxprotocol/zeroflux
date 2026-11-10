#!/bin/sh
# Audience 2: a developer protects THEIR OWN coding-agent sessions.
# The agent's OpenAI-compatible client points at the local ZFX VPN daemon.
# Credentials in files the agent reads are blocked before any model sees them;
# personal details and a registered codename are replaced with placeholders
# and restored in the reply; client config edits are reversible. Then the
# same daemon uses the hosted gateway as protected transport: the hosted side
# only ever sees transformed payloads.
set -eu
. "$(dirname "$0")/../lib.sh"
build
MOCK=$(free_port); PORT=$(free_port); PUB=$(free_port); ADM=$(free_port)
export ZFX_VPN_HOME="$W/zfx-home" ZFX_KEY_BACKEND=file
unset PV_HOME PV_KEY_BACKEND 2>/dev/null || true
Z="$BIN/zfx-vpn"

step "Start a stand-in frontier model that records every byte it receives"
"$Z" mock-upstream --listen "$MOCK" --record "$W/model-received.jsonl" >/dev/null 2>&1 &
PIDS="$PIDS $!"

step "Set up ZFX VPN locally (no detection model download in this demo: deterministic recognizers)"
"$Z" init --non-interactive --port "$PORT" --model none --near-skip --no-notifications \
  --custom-url "http://127.0.0.1:$MOCK/v1" --custom-model mock-echo --custom-ack --default-destination custom >"$W/init.log" 2>&1 || { cat "$W/init.log"; fail "init"; }
printf 'Bluefin-Orchid-42' | "$Z" secret add --port "$PORT" --label "project codename" --action TOKENIZE >/dev/null
KEY=$(cat "$ZFX_VPN_HOME/apps/default.key")
export OPENAI_BASE_URL="http://127.0.0.1:$PORT/v1" OPENAI_API_KEY="$KEY"
"$Z" connect show --port "$PORT" | head -4

step "Agent turn 1: the agent read a .env with a cloud key and a DB password"
OUT=$(python3 -I "$(dirname "$0")/agent_turn.py" leaky); echo "$OUT"
echo "$OUT" | grep -q "ZFX_POLICY_BLOCKED" && pass "credentials blocked on this machine" || fail "not blocked"
[ ! -s "$W/model-received.jsonl" ] && pass "nothing reached the model" || fail "something was sent"

step "Agent turn 2: personal details and the registered codename"
OUT=$(python3 -I "$(dirname "$0")/agent_turn.py" clean); echo "$OUT"
echo "$OUT" | grep -q "sam.okafor@example.dev" && echo "$OUT" | grep -q "Bluefin-Orchid-42" && pass "reply restored for the developer" || fail "not restored"
echo "--- what the model received (tool result):"
tail -n 1 "$W/model-received.jsonl" | python3 -I -c 'import json,sys; m=json.load(sys.stdin)["messages"]; print([x for x in m if x["role"]=="tool"][0]["content"])'
grep -q "sam.okafor@example.dev\|415-555-0123\|Bluefin-Orchid-42" "$W/model-received.jsonl" && fail "raw value reached the model" || pass "model saw placeholders only"

step "Agent turn 3: streamed"
OUT=$(python3 -I "$(dirname "$0")/agent_turn.py" stream); echo "$OUT"
echo "$OUT" | grep -q "Bluefin-Orchid-42" && pass "streamed reply restored" || fail "stream"

step "Point a real agent's config at ZFX VPN, then undo it (temporary HOME)"
mkdir -p "$W/home/.config/opencode"; printf '{"theme":"dark"}\n' >"$W/home/.config/opencode/opencode.json"
HOME="$W/home" "$Z" client configure opencode --port "$PORT" --yes | tail -1
grep -q "127.0.0.1:$PORT" "$W/home/.config/opencode/opencode.json" && pass "opencode now uses the local endpoint" || fail "configure"
HOME="$W/home" "$Z" client rollback opencode --port "$PORT" >/dev/null
[ "$(cat "$W/home/.config/opencode/opencode.json")" = '{"theme":"dark"}' ] && pass "rollback restored the original config" || fail "rollback"

step "Protected transport: same daemon, hosted gateway as upstream, map stays here"
export ZFX_HOSTED_ADMIN_TOKEN="zfxop_demo_operator_token_$(date +%s)_0123456789"
"$BIN/zfx-vpn-hosted" serve --listen "127.0.0.1:$PUB" --admin-listen "127.0.0.1:$ADM" >"$W/hosted.log" 2>&1 &
PIDS="$PIDS $!"
wait_http "http://127.0.0.1:$PUB/healthz"
TID=$(curl -s "http://127.0.0.1:$ADM/admin/v1/tenants" -H "Authorization: Bearer $ZFX_HOSTED_ADMIN_TOKEN" -H 'Content-Type: application/json' -d "{\"name\":\"dev\",\"upstreams\":[{\"schema_version\":1,\"id\":\"frontier\",\"kind\":\"OPENAI_COMPATIBLE\",\"display_name\":\"frontier\",\"base_url\":\"http://127.0.0.1:$MOCK/v1\",\"default_model\":\"mock-echo\",\"declared_tier\":\"EXTERNAL_UNVERIFIED\",\"enabled\":true,\"allow_failover\":false,\"supports_responses\":true,\"timeout_secs\":30}],\"default_destination\":\"frontier\"}" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["tenant_id"])')
HKEY=$(curl -s "http://127.0.0.1:$ADM/admin/v1/tenants/$TID/keys" -H "Authorization: Bearer $ZFX_HOSTED_ADMIN_TOKEN" -H 'Content-Type: application/json' -d '{"scopes":["inference"]}' | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["key"])')
printf '%s' "$HKEY" | "$Z" destination add-hosted zfx-hosted "http://127.0.0.1:$PUB/v1" frontier/mock-echo --default --port "$PORT"
: >"$W/model-received.jsonl"
OUT=$(python3 -I "$(dirname "$0")/agent_turn.py" clean); echo "$OUT"
echo "$OUT" | grep -q "sam.okafor@example.dev" && pass "reply restored locally" || fail "protected transport restore"
grep -q "sam.okafor@example.dev\|Bluefin-Orchid-42" "$W/model-received.jsonl" && fail "raw value reached the hosted side's model" || pass "hosted side and its model saw only transformed payloads"
grep -q "x-zfx-preprocessed\|preprocessed" "$W/model-received.jsonl" && fail "zfx extension leaked to the model" || pass "zfx_privacy hints were stripped before the model"
"$Z" stop --port "$PORT" >/dev/null
echo
echo "developer demo: all checks passed"
