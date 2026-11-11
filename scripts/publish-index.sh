#!/usr/bin/env bash
# Rebuild zeroflux.network with the latest published ZFX Markets index and deploy it.
# Run by launchd (deploy/launchd/network.zeroflux.publish-index.plist) after each UTC day lands,
# or by hand. The index poller itself runs as network.zeroflux.index-poller.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="$HOME/.npm-global/bin:$HOME/.local/bin:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
LOG="$HOME/.local/state/zfx-index/publish.log"
mkdir -p "$(dirname "$LOG")"
{
  echo "== $(date -u +%FT%TZ) publish"
  cd "$ROOT"
  .venv/bin/python site/build.py
  cd site/worker && cf deploy 2>&1 | tail -2
  echo "== done"
} >>"$LOG" 2>&1
