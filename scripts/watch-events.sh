#!/usr/bin/env bash
# Subscribe to live argus events via WebSocket.
# Usage: ./scripts/watch-events.sh [host:port]
#
# Requires: websocat (brew install websocat) or wscat (npm i -g wscat)
set -euo pipefail

ADDR="${1:-localhost:9090}"

if command -v websocat >/dev/null 2>&1; then
    exec websocat "ws://$ADDR/ws" | jq -c '{seq, type, pid, path: (.path // null)}'
elif command -v wscat >/dev/null 2>&1; then
    exec wscat -c "ws://$ADDR/ws" | jq -c '{seq, type, pid, path: (.path // null)}'
else
    echo "Install websocat or wscat:"
    echo "  brew install websocat"
    echo "  npm i -g wscat"
    exit 1
fi
