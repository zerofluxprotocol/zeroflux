#!/bin/sh
# Tests for the promptvpn-setup skill scripts. Runs in a temporary HOME and
# PV_HOME; never touches the user's real configuration.
#   PROMPTVPN_BIN=target/debug/promptvpn sh skills/promptvpn-setup/tests/run.sh
set -eu
root=$(cd "$(dirname "$0")/../../.." && pwd)
skill="$root/skills/promptvpn-setup"
PV=${PROMPTVPN_BIN:-$root/target/debug/promptvpn}
[ -x "$PV" ] || { echo "build first: cargo build -p pv-cli" >&2; exit 1; }
export PROMPTVPN_BIN="$PV"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
pass=0; fail=0
ok() { pass=$((pass+1)); echo "ok   - $1"; }
no() { fail=$((fail+1)); echo "FAIL - $1"; }

# 1. discover: valid JSON, read-only
HOME="$tmp/home" sh "$skill/scripts/discover.sh" > "$tmp/d.json"
python3 -I -c "import json,sys; d=json.load(open(sys.argv[1])); assert d['os'] in ('macos','linux'); assert len(d['clients'])==4" "$tmp/d.json" && ok "discover emits valid JSON" || no "discover JSON"
[ ! -e "$tmp/home" ] && ok "discover wrote nothing" || no "discover created files"

# 2. installer script is valid shell and refuses unsupported options safely
sh -n "$root/install.sh" && ok "install.sh parses" || no "install.sh syntax"
grep -q 'sudo' "$root/install.sh" && grep -qv 'never uses sudo' "$root/install.sh" && grep -n 'sudo ' "$root/install.sh" | grep -v '#' >/dev/null && no "install.sh uses sudo" || ok "install.sh never uses sudo"

# 3. egress test passes (synthetic secrets never reach the mock)
sh "$skill/scripts/egress_test.sh" > "$tmp/egress.txt" 2>&1 && grep -q PASS "$tmp/egress.txt" && ok "egress test passes" || { cat "$tmp/egress.txt"; no "egress test"; }

# 4. client configure is reversible and idempotent (temp HOME and PV_HOME)
export PV_HOME="$tmp/pvhome"; mkdir -p "$PV_HOME/apps"; printf 'pvk_test' > "$PV_HOME/apps/default.key"
mkdir -p "$tmp/h/.config/opencode"; printf '{"theme": "dark"}\n' > "$tmp/h/.config/opencode/opencode.json"
orig=$(cat "$tmp/h/.config/opencode/opencode.json")
HOME="$tmp/h" "$PV" --port 1 client configure opencode --yes >/dev/null
HOME="$tmp/h" "$PV" --port 1 client configure opencode --yes >/dev/null
grep -q promptvpn "$tmp/h/.config/opencode/opencode.json" && grep -q '"theme"' "$tmp/h/.config/opencode/opencode.json" && ok "opencode configured, existing keys kept" || no "opencode configure"
HOME="$tmp/h" sh "$skill/scripts/rollback.sh" opencode >/dev/null
[ "$(cat "$tmp/h/.config/opencode/opencode.json")" = "$orig" ] && ok "rollback restores original after repeated configure" || no "rollback"
set +e; HOME="$tmp/h" "$PV" --port 1 client configure vscode --yes >/dev/null 2>&1; rc=$?; set -e
[ $rc -ne 0 ] && ok "unsupported client refused" || no "unsupported client accepted"

echo "$pass passed, $fail failed"
[ $fail -eq 0 ]
