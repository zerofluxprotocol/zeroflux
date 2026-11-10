#!/bin/sh
# Synthetic egress test: proves protected data is blocked/transformed before it
# reaches an upstream, using a temporary instance and a local mock that records
# exact bytes. Uses synthetic values only. Exit 0 = pass, 2 = protection failure.
set -eu
PV=${PROMPTVPN_BIN:-promptvpn}
out=$("$PV" connect test) || { echo "$out"; echo "PROTECTION TEST FAILED: stop setup and report to the user." >&2; exit 2; }
echo "$out"
echo "$out" | grep -q '"passed": true' || { echo "PROTECTION TEST FAILED" >&2; exit 2; }
echo "PASS: synthetic SSN and API key never reached the mock upstream; email was tokenized and restored."
