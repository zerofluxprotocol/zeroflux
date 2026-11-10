#!/bin/sh
# Restore a client's original config from Prompt VPN's backup.
set -eu
[ $# -eq 1 ] || { echo "usage: rollback.sh <codex|aider|opencode>" >&2; exit 64; }
"${PROMPTVPN_BIN:-promptvpn}" client rollback "$1"
