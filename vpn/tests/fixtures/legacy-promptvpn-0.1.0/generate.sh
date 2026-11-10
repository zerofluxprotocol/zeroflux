#!/bin/sh
# Regenerate the legacy install fixture with the ORIGINAL Prompt VPN binary.
#   sh generate.sh <path to promptvpn 0.1.0 binary> <output dir>
# The binary must be built from the import commit (promptvpn d2ac753), e.g.
#   git archive 499f9f6:vpn | tar -x -C /tmp/pv-old && (cd /tmp/pv-old && cargo build -p pv-cli)
# All values are synthetic. The file key backend is used so the data key is
# part of the fixture; the keychain path is covered by tests/migration/keychain_e2e.sh.
set -eu
B=$1; OUT=$2
free_port() { python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])'; }
P=$(free_port); M=$(free_port)
export PV_HOME="$OUT/home" PV_KEY_BACKEND=file
"$B" mock-upstream --listen "$M" --record "$OUT/rec.jsonl" >"$OUT/mock.log" 2>&1 &
MOCK=$!
trap 'kill $MOCK 2>/dev/null || true' EXIT
"$B" init --non-interactive --port "$P" --model none --near-skip \
  --custom-url "http://127.0.0.1:$M/v1" --custom-model m --custom-ack \
  --categories EMAIL=TOKENIZE --no-notifications >/dev/null
printf 'Nightjar-Cedar-77' | "$B" secret add --port "$P" --label "project codename" --action TOKENIZE
printf 'zz-legacy-blocked-value-991' | "$B" secret add --port "$P" --label "legacy blocked"
K=$(cat "$PV_HOME/apps/default.key")
curl -s "http://127.0.0.1:$P/v1/chat/completions" -H "Authorization: Bearer $K" -H 'Content-Type: application/json' \
  -H 'X-PromptVPN-Conversation: fixture-conv-1' \
  -d '{"model":"m","messages":[{"role":"user","content":"Email alice.fixture@example.test about Nightjar-Cedar-77"}]}' >/dev/null
curl -s "http://127.0.0.1:$P/v1/chat/completions" -H "Authorization: Bearer $K" -H 'Content-Type: application/json' \
  -d '{"model":"m","messages":[{"role":"user","content":"my ssn is 123-45-6789"}]}' >/dev/null
"$B" policy category PHONE REDACT --port "$P" >/dev/null
"$B" connect add-app fixture-agent --port "$P" >/dev/null
"$B" entity add --port "$P" --label "insurance policy number" >/dev/null
"$B" policy export --port "$P" >"$OUT/policy-export.json"
"$B" secret list --port "$P" --json >"$OUT/secrets-list.json"
"$B" stop --port "$P" >/dev/null
rm -f "$PV_HOME/daemon.log" "$PV_HOME"/promptvpn.db-shm "$PV_HOME"/promptvpn.db-wal
