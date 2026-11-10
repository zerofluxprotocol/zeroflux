#!/bin/sh
# Audience 1: a builder protects THEIR END USERS' data.
# A support app sends end-user messages through the ZFX VPN hosted gateway.
# The model provider (a local recording mock here) only sees placeholders; the
# end user gets the reply with their details restored; the hosted gateway
# discloses that its operator can see raw prompts in this mode.
set -eu
. "$(dirname "$0")/../lib.sh"
build
MOCK=$(free_port); PUB=$(free_port); ADM=$(free_port); APP=$(free_port)
export ZFX_HOSTED_ADMIN_TOKEN="zfxop_demo_operator_token_$(date +%s)_0123456789"

step "Start a stand-in model provider that records every byte it receives"
"$BIN/zfx-vpn" mock-upstream --listen "$MOCK" --record "$W/model-received.jsonl" >/dev/null 2>&1 &
PIDS="$PIDS $!"
step "Start the ZFX VPN hosted gateway (memory only)"
"$BIN/zfx-vpn-hosted" serve --listen "127.0.0.1:$PUB" --admin-listen "127.0.0.1:$ADM" >"$W/hosted.log" 2>&1 &
PIDS="$PIDS $!"
wait_http "http://127.0.0.1:$PUB/healthz"

step "Operator: create the builder's tenant and an API key"
T=$(curl -s "http://127.0.0.1:$ADM/admin/v1/tenants" -H "Authorization: Bearer $ZFX_HOSTED_ADMIN_TOKEN" -H 'Content-Type: application/json' -d "{\"name\":\"acme-support\",\"upstreams\":[{\"schema_version\":1,\"id\":\"frontier\",\"kind\":\"OPENAI_COMPATIBLE\",\"display_name\":\"frontier model\",\"base_url\":\"http://127.0.0.1:$MOCK/v1\",\"default_model\":\"mock-echo\",\"declared_tier\":\"EXTERNAL_UNVERIFIED\",\"enabled\":true,\"allow_failover\":false,\"supports_responses\":true,\"timeout_secs\":30}],\"default_destination\":\"frontier\",\"quota\":{\"requests_per_minute\":60,\"requests_per_month\":1000}}")
TID=$(echo "$T" | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["tenant_id"])')
KEY=$(curl -s "http://127.0.0.1:$ADM/admin/v1/tenants/$TID/keys" -H "Authorization: Bearer $ZFX_HOSTED_ADMIN_TOKEN" -H 'Content-Type: application/json' -d '{"scopes":["inference","privacy:read"]}' | python3 -I -c 'import json,sys; print(json.load(sys.stdin)["key"])')
echo "tenant $TID, key ${KEY%"${KEY#????????????}"}..."
curl -s "http://127.0.0.1:$PUB/v1/zfx/privacy-policy" | python3 -I -c 'import json,sys; d=json.load(sys.stdin)["modes"]["hosted"]; print("disclosure:", d["disclosure"][:150], "..."); assert d["operator_sees_raw_prompts"] is True' && pass "hosted mode discloses that the operator sees raw prompts"

step "Start the builder's support app, pointed at ZFX instead of the provider"
ZFX_BASE_URL="http://127.0.0.1:$PUB/v1" ZFX_API_KEY="$KEY" python3 -I "$(dirname "$0")/support_app.py" "$APP" &
PIDS="$PIDS $!"
sleep 0.5

step "An end user writes in with their own details"
MSG='Hi, I am Priya Raman, my email is priya.raman@example.com and my phone is 415-555-0177. My order has not arrived.'
R=$(curl -s "http://127.0.0.1:$APP/" -d "{\"message\": \"$MSG\"}")
echo "$R" | python3 -I -m json.tool
echo "$R" | grep -q "priya.raman@example.com" && pass "end user's reply has their email restored" || fail "reply not restored"
echo "$R" | grep -q "TRANSFORMED_PUBLIC" && pass "route reported as TRANSFORMED_PUBLIC" || fail "route header"
echo "$R" | grep -q "operator-sees-raw-prompt" && pass "trust header shown to the app" || fail "trust header"
echo "--- what the model provider received:"
tail -n 1 "$W/model-received.jsonl"
grep -q "priya.raman@example.com\|415-555-0177" "$W/model-received.jsonl" && fail "end-user data reached the model" || pass "model provider received placeholders only"

step "Same thing, streamed"
R=$(curl -s "http://127.0.0.1:$APP/" -d "{\"message\": \"Ship to priya.raman@example.com please\", \"stream\": true}")
echo "$R" | grep -q "priya.raman@example.com" && pass "streamed reply restored" || fail "stream: $R"

step "An end user pastes a card number"
R=$(curl -s "http://127.0.0.1:$APP/" -d '{"message": "charge my card 4111 1111 1111 1111"}')
echo "$R"
echo "$R" | grep -q "ZFX_POLICY_BLOCKED" && pass "critical value blocked before the model" || fail "card not blocked"
grep -q "4111" "$W/model-received.jsonl" && fail "card reached the model" || pass "card never reached the model"

step "Usage accounting for the tenant"
curl -s "http://127.0.0.1:$ADM/admin/v1/tenants/$TID/usage" -H "Authorization: Bearer $ZFX_HOSTED_ADMIN_TOKEN"; echo
echo
echo "builder demo: all checks passed"
