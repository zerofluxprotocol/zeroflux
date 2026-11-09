#!/usr/bin/env bash
# Fetch the official NEAR Intents Verifier contract (near/intents, MIT) at the
# pinned release and verify it against the release's published SHA-256.
set -euo pipefail
TAG=${TAG:-defuse/v0.4.2}
DIR="$(cd "$(dirname "$0")" && pwd)/vendor"
mkdir -p "$DIR"
cd "$DIR"
gh release download "$TAG" -R near/intents -p defuse.wasm -p defuse.sha256 --clobber
want=$(awk '{print $1}' defuse.sha256)
got=$(shasum -a 256 defuse.wasm | awk '{print $1}')
if [ "$want" != "$got" ]; then echo "SHA-256 mismatch: release says $want, got $got" >&2; exit 1; fi
echo "{\"tag\":\"$TAG\",\"sha256\":\"$got\",\"bytes\":$(wc -c < defuse.wasm)}" | tee verifier.json
