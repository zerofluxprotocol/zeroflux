#!/usr/bin/env bash
# Install the two launchd jobs that keep the ZFX Markets index fresh on this Mac:
#   network.zeroflux.index-poller   the poller (always on, restarted if it exits)
#   network.zeroflux.publish-index  rebuild + deploy zeroflux.network twice a day
# Undo: launchctl bootout gui/$(id -u)/<label>; rm ~/Library/LaunchAgents/<label>.plist
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
mkdir -p "$HOME/Library/LaunchAgents" "$HOME/.local/state/zfx-index"
for l in network.zeroflux.index-poller network.zeroflux.publish-index; do
  dst="$HOME/Library/LaunchAgents/$l.plist"
  sed -e "s#__HOME__#$HOME#g" -e "s#__ROOT__#$ROOT#g" "$ROOT/deploy/launchd/$l.plist" > "$dst"
  launchctl bootout "gui/$(id -u)/$l" 2>/dev/null || true
  launchctl bootstrap "gui/$(id -u)" "$dst"
  echo "loaded $l"
done
