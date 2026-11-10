# Shared helpers for the demos (POSIX sh). Everything runs on 127.0.0.1 with
# ephemeral ports, temporary data directories and synthetic data.
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
. "$HOME/.cargo/env" 2>/dev/null || true
BIN="$ROOT/target/debug"
W=$(mktemp -d "${TMPDIR:-/tmp}/zfx-demo.XXXXXX")
PIDS=""
cleanup() {
  for p in $PIDS; do kill "$p" 2>/dev/null || true; done
  [ -n "${ZFX_VPN_HOME:-}" ] && [ -f "$ZFX_VPN_HOME/daemon.pid" ] && kill "$(cat "$ZFX_VPN_HOME/daemon.pid")" 2>/dev/null || true
}
trap cleanup EXIT
free_port() { python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])'; }
step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
pass() { echo "PASS $*"; }
fail() { echo "FAIL $*"; exit 1; }
wait_http() { for _ in $(seq 1 100); do curl -s -o /dev/null "$1" && return 0; sleep 0.1; done; fail "service at $1 did not start"; }
build() { (cd "$ROOT" && cargo build -q -p pv-cli -p zfx-vpn-hosted --locked); }
