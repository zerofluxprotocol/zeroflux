#!/usr/bin/env bash
# Rebuild zfx-perps-sdk.js (the browser bundle of @zeroflux/zfx-perps-sdk used by venue.js).
#   markets/apps/trade/build-sdk.sh
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
SDK="$HERE/../../zfx-perps/packages/sdk-ts"
(cd "$SDK" && pnpm install --frozen-lockfile >/dev/null)
TMP="$(mktemp -d)"
cat > "$TMP/entry.ts" <<TS
export { ZfxPerpsClient, ClearingApiError } from "$SDK/src/clearing/client.js";
export { KeySigner } from "$SDK/src/signer.js";
export * as clearing from "$SDK/src/clearing/index.js";
TS
(cd "$SDK" && npx -y esbuild@0.24 "$TMP/entry.ts" --bundle --format=esm --platform=browser --target=es2020 --minify --outfile="$HERE/zfx-perps-sdk.js")
rm -rf "$TMP"
