#!/bin/sh
# Run both demos; exit non-zero if any check fails.
set -eu
d=$(cd "$(dirname "$0")" && pwd)
sh "$d/builder-app/run.sh"
sh "$d/developer-agent/run.sh"
