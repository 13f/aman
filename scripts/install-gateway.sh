#!/bin/bash
# Build and install the aman gateway binary to ~/.aman/bin/
#
# Usage: ./scripts/install-gateway.sh [--release|--debug] [--run]

set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="release"
RUN=false

for arg in "$@"; do
    case "$arg" in
        --release) PROFILE="release" ;;
        --debug)   PROFILE="debug" ;;
        --run)     RUN=true ;;
        *)         echo "Unknown flag: $arg"; exit 1 ;;
    esac
done

SRC="target/${PROFILE}/aman"
DEST_DIR="$HOME/.aman/bin"
DEST="$DEST_DIR/aman"

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

if $RUN; then
    echo ""
    echo "==> Starting gateway..."
    exec "$DEST"
fi
