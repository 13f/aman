#!/bin/bash
# Build, sign, install, and launch the aman gateway.
#
# Usage: ./scripts/gateway.sh [--no-launch]
#   --no-launch  Build + sign only, don't start the gateway.

set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="release"
SRC="target/${PROFILE}/gateway"
DEST="$HOME/.aman/bin/gateway"

echo "==> Building gateway (${PROFILE})..."
cargo build --${PROFILE} -p gateway

echo ""
echo "==> Installing + signing..."
mkdir -p "$HOME/.aman/bin"

# Kill any running gateway to free the port
lsof -ti :9999 2>/dev/null | xargs kill -9 2>/dev/null || true

cp -f "$SRC" "$DEST"
codesign -s - -f "$DEST" 2>/dev/null

echo "     Installed: $DEST"
codesign -dv "$DEST" 2>&1 | grep -E "Identifier|Signature" | sed 's/^/     /'

if [[ "${1:-}" == "--no-launch" ]]; then
    echo ""
    echo "==> Skipping launch (--no-launch)"
    exit 0
fi

echo ""
echo "==> Launching gateway..."
exec "$DEST"
