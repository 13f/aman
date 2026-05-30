#!/bin/bash
# Sign the gateway binary and install to a stable path.
# Running from a stable path prevents macOS from treating
# each rebuild as a new, unknown binary for keychain access.
#
# Usage: ./scripts/sign-gateway.sh [release|debug]
#   release (default): signs target/release/gateway
#   debug:            signs target/debug/gateway

set -euo pipefail

PROFILE="${1:-release}"
SRC="target/${PROFILE}/gateway"
DEST="$HOME/.aman/bin/gateway"

if [ ! -f "$SRC" ]; then
    echo "Error: $SRC not found. Build first: cargo build --${PROFILE} -p gateway"
    exit 1
fi

mkdir -p "$HOME/.aman/bin"

# Copy to stable path (preserves inode across identical copies)
cp -f "$SRC" "$DEST"

# Ad-hoc sign — macOS remembers keychain authorizations per-signature.
# Ad-hoc signatures are derived from the binary hash, so the same binary
# always gets the same signature (unlike unsigned binaries where the
# inode changes on each copy).
codesign -s - -f "$DEST" 2>/dev/null

echo "Signed and installed: $DEST"
echo ""
echo "Run with: $DEST"
echo ""
echo "Tip: add this alias to your shell:"
echo "  alias gw='$DEST'"
