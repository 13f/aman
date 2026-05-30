#!/bin/bash
# Build and launch the aman gateway.
#
# Usage: ./scripts/gateway.sh [--no-launch]

set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="release"
SRC="target/${PROFILE}/gateway"
DEST="$HOME/.aman/bin/gateway"

echo "==> Building gateway (${PROFILE})..."
cargo build --${PROFILE} -p gateway

echo ""
echo "==> Installing..."
mkdir -p "$HOME/.aman/bin"

lsof -ti :9999 2>/dev/null | xargs kill -9 2>/dev/null || true

cp -f "$SRC" "$DEST"
codesign -s - -f "$DEST" 2>/dev/null || true

echo "     Installed: $DEST"

if [[ "${1:-}" == "--no-launch" ]]; then
    echo ""
    echo "==> Skipping launch (--no-launch)"
    exit 0
fi

echo ""
echo "==> Launching..."
exec "$DEST"
