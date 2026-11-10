#!/bin/sh
# Gate 1 end to end with real binaries: a Prompt VPN 0.1 install created by the
# OLD binary is upgraded by `zfx-vpn` (invoked through the deprecated
# `promptvpn` name), used, rolled back, and then served again by the OLD binary.
#
#   sh tests/migration/upgrade_rollback.sh [path to promptvpn 0.1 binary]
#
# Without an argument the old binary is built from the import commit (499f9f6),
# which is the Prompt VPN 0.1 source as it was before the rename.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
. "$HOME/.cargo/env" 2>/dev/null || true
W=$(mktemp -d "${TMPDIR:-/tmp}/zfx-upgrade.XXXXXX")
PIDS=""
cleanup() {
  for p in $PIDS; do kill "$p" 2>/dev/null || true; done
  # Daemons this script started (pid files in its own temp data dir only).
  for f in "$W/fx/home/daemon.pid"; do [ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null || true; done
}
trap cleanup EXIT
pass() { echo "PASS $1"; }
fail() { echo "FAIL $1"; exit 1; }
free_port() { python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])'; }

OLD=${1:-}
if [ -z "$OLD" ]; then
  mkdir -p "$W/old-src"
  git -C "$(git -C "$ROOT" rev-parse --show-toplevel)" archive 499f9f6:vpn | tar -x -C "$W/old-src"
  (cd "$W/old-src" && CARGO_TARGET_DIR="${ZFX_OLD_TARGET:-$W/old-target}" cargo build -q -p pv-cli --locked)
  OLD="${ZFX_OLD_TARGET:-$W/old-target}/debug/promptvpn"
fi
(cd "$ROOT" && cargo build -q -p pv-cli --locked)
NEW="$ROOT/target/debug/zfx-vpn"
mkdir -p "$W/bin" && ln -s "$NEW" "$W/bin/promptvpn"

# 1. Legacy install written by the old binary.
mkdir -p "$W/fx"
sh "$ROOT/tests/fixtures/legacy-promptvpn-0.1.0/generate.sh" "$OLD" "$W/fx" >/dev/null 2>&1
read P M <"$W/fx/ports"
H="$W/fx/home"
[ -f "$H/promptvpn.db" ] || fail "old binary created promptvpn.db"
export PV_HOME="$H" PV_KEY_BACKEND=file
unset ZFX_VPN_HOME ZFX_KEY_BACKEND 2>/dev/null || true
"$NEW" mock-upstream --listen "$M" --record "$W/rec.jsonl" >/dev/null 2>&1 &
PIDS="$PIDS $!"
sleep 0.5
KEY=$(cat "$H/apps/default.key")
chat() { curl -s "http://127.0.0.1:$P/v1/chat/completions" -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' -d "{\"model\":\"m\",\"messages\":[{\"role\":\"user\",\"content\":\"$1\"}]}"; }
same_rules() { python3 -I -c 'import json,sys; a=json.load(open(sys.argv[1]))["rules"]; b=json.load(open(sys.argv[2]))["rules"]; sys.exit(0 if a==b else 1)' "$1" "$2"; }
same_secrets() { python3 -I -c 'import json,sys; f=lambda p: sorted((s["secret_id"],s["label"],s["action"]) for s in json.load(open(p))["secrets"]); sys.exit(0 if f(sys.argv[1])==f(sys.argv[2]) else 1)' "$1" "$2"; }

# 2. Upgrade through the deprecated name.
"$W/bin/promptvpn" start --port "$P" >"$W/start.out" 2>"$W/start.err" || { cat "$W/start.err"; fail "zfx-vpn start via promptvpn alias"; }
grep -q "deprecated" "$W/start.err" && pass "promptvpn alias runs zfx-vpn and prints a deprecation note" || fail "deprecation note"
grep -q "Migrated Prompt VPN data" "$W/start.err" && pass "first run migrated the install" || fail "migration message"
[ -f "$H/zfx-vpn.db" ] && [ ! -f "$H/promptvpn.db" ] && pass "database renamed to zfx-vpn.db" || fail "database rename"
"$NEW" policy export --port "$P" >"$W/policy-new.json"
same_rules "$W/policy-new.json" "$W/fx/policy-export.json" && pass "policy identical after upgrade" || fail "policy after upgrade"
"$NEW" secret list --port "$P" --json >"$W/secrets-new.json"
same_secrets "$W/secrets-new.json" "$W/fx/secrets-list.json" && pass "registered secrets identical after upgrade" || fail "secrets after upgrade"
# The fixture has a custom entity type but no detection model: requests must
# still fail closed to review, exactly as under Prompt VPN 0.1.
R=$(chat "Email alice.fixture@example.test about Nightjar-Cedar-77")
echo "$R" | grep -q "ZFX_APPROVAL_REQUIRED" && echo "$R" | grep -q "CLASSIFIER_FAILURE" && pass "custom entity survived; fail-closed review preserved" || fail "review after upgrade: $R"
[ ! -s "$W/rec.jsonl" ] && pass "nothing reached upstream while held" || fail "held request leaked"
ENT=$("$NEW" entity list --port "$P" --json | python3 -I -c 'import json,sys; v=json.load(sys.stdin); e=v.get("entities", v) if isinstance(v, dict) else v; print(e[0]["entity"]["id"] if "entity" in e[0] else e[0]["id"])')
"$NEW" entity remove "$ENT" --port "$P" >/dev/null
"$NEW" policy export --port "$P" >"$W/policy-final.json"
R=$(chat "Email alice.fixture@example.test about Nightjar-Cedar-77")
echo "$R" | grep -q "alice.fixture@example.test" && pass "old app key works; reply restored" || fail "request after upgrade: $R"
tail -n 1 "$W/rec.jsonl" | grep -q "alice.fixture" && fail "raw email reached upstream" || pass "upstream saw no raw email"
tail -n 1 "$W/rec.jsonl" | grep -q '\[\[ZFX_EMAIL_' && pass "upstream saw a ZFX placeholder" || fail "placeholder upstream"
R=$(chat "my ssn is 123-45-6789")
echo "$R" | grep -q "ZFX_POLICY_BLOCKED" && pass "critical value still blocked" || fail "block after upgrade: $R"
"$NEW" stop --port "$P" >/dev/null

# 3. Rollback, then the OLD binary serves the same install.
"$NEW" migrate rollback --yes >"$W/rollback.out" 2>&1 || { cat "$W/rollback.out"; fail "rollback"; }
[ -f "$H/promptvpn.db" ] && [ ! -f "$H/zfx-vpn.db" ] && pass "rollback restored promptvpn.db" || fail "rollback file"
"$OLD" start --port "$P" >/dev/null 2>"$W/old.err" || { cat "$W/old.err"; fail "old binary start after rollback"; }
pass "Prompt VPN 0.1 starts on the rolled-back install"
"$OLD" policy export --port "$P" >"$W/policy-old.json"
same_rules "$W/policy-old.json" "$W/policy-final.json" && pass "policy identical after rollback, including the change made under ZFX VPN (old binary)" || fail "policy after rollback"
"$OLD" secret list --port "$P" --json >"$W/secrets-old.json"
same_secrets "$W/secrets-old.json" "$W/fx/secrets-list.json" && pass "secrets identical after rollback (old binary)" || fail "secrets after rollback"
R=$(chat "Email bob.fixture@example.test")
echo "$R" | grep -q "bob.fixture@example.test" && pass "old binary proxies with the old app key after rollback" || fail "old request: $R"
R=$(chat "my ssn is 123-45-6789")
echo "$R" | grep -q "PV_POLICY_BLOCKED" && pass "old binary still blocks critical values" || fail "old block: $R"
"$OLD" stop --port "$P" >/dev/null
echo "all upgrade/rollback checks passed ($W)"
