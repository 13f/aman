#!/bin/bash
# Build and install the aman gateway binary to ~/.aman/bin/
#
# Usage: ./scripts/install-gateway.sh [--release|--debug]

set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="${1:-"--release"}"
PROFILE="${PROFILE#--}"  # strip leading --

SRC="target/${PROFILE}/gateway"
DEST_DIR="$HOME/.aman/bin"
DEST="$DEST_DIR/gateway"

echo "==> Building gateway (${PROFILE})..."
cargo build --"${PROFILE}" -p gateway

echo "==> Installing..."
mkdir -p "$DEST_DIR"

# Kill any running gateway before overwriting
lsof -ti :9999 2>/dev/null | xargs kill -9 2>/dev/null || true

cp -f "$SRC" "$DEST"

echo "     Installed: $DEST"
echo ""
echo "==> Done. The Tauri app can now start the gateway from $DEST"
