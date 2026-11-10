#!/usr/bin/env bash
# Build the NEAR contracts to out/*.wasm with cargo-near.
set -euo pipefail
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
cd "$(dirname "$0")/../contracts"
for c in zfx-perps-settlement zfx-perps-clearing mock-ft; do
  (cd "$c" && cargo near build non-reproducible-wasm --no-abi >/dev/null)
done
mkdir -p ../out
cp target/near/zfx_perps_settlement/zfx_perps_settlement.wasm ../out/
cp target/near/mock_ft/mock_ft.wasm ../out/
cp target/near/zfx_perps_clearing_contract/zfx_perps_clearing_contract.wasm ../out/zfx_perps_clearing.wasm
ls -la ../out/*.wasm
