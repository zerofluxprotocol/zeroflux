#!/usr/bin/env bash
# Build the NEAR contracts to out/*.wasm with cargo-near.
set -euo pipefail
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
cd "$(dirname "$0")/../contracts"
for c in obkit-settlement mock-ft; do
  (cd "$c" && cargo near build non-reproducible-wasm --no-abi >/dev/null)
done
mkdir -p ../out
cp target/near/obkit_settlement/obkit_settlement.wasm ../out/
cp target/near/mock_ft/mock_ft.wasm ../out/
ls -la ../out/*.wasm
