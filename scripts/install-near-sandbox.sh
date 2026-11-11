#!/usr/bin/env bash
# Installs the nearcore sandbox binary that the markets tests (index oracle, zfx-perps) start.
# Destination: ~/.local/share/zeroflux/near-sandbox (override with ZFX_TOOLS).
# macOS builds link liblzma; without Homebrew xz, a private copy is built next to the binary.
# The mining end-to-end tests do not need this: zfx-bridge downloads its own sandbox.
set -euo pipefail
VERSION="${NEAR_SANDBOX_VERSION:-2.14.0}"
DEST="${ZFX_TOOLS:-$HOME/.local/share/zeroflux}"
mkdir -p "$DEST"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) PLATFORM="Darwin-arm64" ;;
  Linux-x86_64) PLATFORM="Linux-x86_64" ;;
  Linux-aarch64) PLATFORM="Linux-aarch64" ;;
  *) echo "unsupported platform $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac
if [ ! -x "$DEST/near-sandbox" ]; then
  TMP=$(mktemp -d)
  curl -fsSL "https://s3-us-west-1.amazonaws.com/build.nearprotocol.com/nearcore/$PLATFORM/$VERSION/near-sandbox.tar.gz" -o "$TMP/s.tgz"
  tar xzf "$TMP/s.tgz" -C "$TMP"
  find "$TMP" -name near-sandbox -type f -exec cp {} "$DEST/near-sandbox" \;
  chmod +x "$DEST/near-sandbox"
fi
if [ "$(uname -s)" = "Darwin" ] && [ ! -f /opt/homebrew/opt/xz/lib/liblzma.5.dylib ] && [ ! -f "$DEST/xz/lib/liblzma.5.dylib" ]; then
  TMP=$(mktemp -d)
  curl -fsSL https://github.com/tukaani-project/xz/releases/download/v5.6.4/xz-5.6.4.tar.gz -o "$TMP/xz.tgz"
  tar xzf "$TMP/xz.tgz" -C "$TMP"
  (cd "$TMP/xz-5.6.4" && ./configure --prefix="$DEST/xz" --disable-static --disable-doc --disable-xz --disable-xzdec \
     --disable-lzmadec --disable-lzmainfo --disable-scripts >/dev/null && make -j4 >/dev/null && make install >/dev/null)
fi
echo "near-sandbox: $DEST/near-sandbox"
