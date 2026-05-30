#!/bin/bash
# Build, sign, install, and launch the aman gateway.
#
# Usage: ./scripts/gateway.sh [--no-launch]
#
# Reduce keychain prompts by exporting API keys as env vars:
#   source scripts/gateway.sh --export-env
# This prints export commands; add to ~/.zshrc to skip keychain prompts.

set -euo pipefail
cd "$(dirname "$0")/.."

PROFILE="release"
SRC="target/${PROFILE}/gateway"
DEST="$HOME/.aman/bin/gateway"

# ── --export-env: print env vars to source ─────────────────────────
if [[ "${1:-}" == "--export-env" ]]; then
    echo "# Add these to your ~/.zshrc to skip keychain prompts during dev:"
    security dump-keychain ~/Library/Keychains/login.keychain-db 2>/dev/null \
        | grep -oP '"labl"<blob>="aman\.[^"]*"' \
        | sed 's/"labl"<blob>="//;s/"//' \
        | sort -u \
        | while read -r item; do
        # Map keychain keys to env var names
        case "$item" in
            aman.providers.*.api_key)
                provider=$(echo "$item" | sed 's/aman\.providers\.\(.*\)\.api_key/\1/' | tr '[:lower:]' '[:upper:]' | tr -c 'A-Z0-9_' '_')
                echo "export AMAN_PROVIDER_${provider}_API_KEY='changeme'"
                ;;
            aman.bot.telegram.*.token)
                echo "# telegram bot token: $item"
                ;;
            *)
                echo "# $item"
                ;;
        esac
    done
    exit 0
fi

echo "==> Building gateway (${PROFILE})..."
cargo build --${PROFILE} -p gateway

echo ""
echo "==> Installing..."
mkdir -p "$HOME/.aman/bin"

# Kill any running gateway
lsof -ti :9999 2>/dev/null | xargs kill -9 2>/dev/null || true

cp -f "$SRC" "$DEST"
codesign -s - -f "$DEST" 2>/dev/null

# ── One-time keychain ACL setup ────────────────────────────────────
# Run this only if the marker file doesn't exist (first run after build).
ACL_DONE="$HOME/.aman/.keychain-acl-done"
if [ ! -f "$ACL_DONE" ]; then
    echo ""
    echo "==> One-time: authorizing keychain access..."
    echo "    (macOS may ask for your password — grant 'Always Allow')"
    security dump-keychain ~/Library/Keychains/login.keychain-db 2>/dev/null \
        | grep -oP '"labl"<blob>="aman\.[^"]*"' \
        | sed 's/"labl"<blob>="//;s/"//' \
        | sort -u \
        | while read -r item; do
        echo "     $item"
    done
    touch "$ACL_DONE"
fi

if [[ "${1:-}" == "--no-launch" ]]; then
    echo ""
    echo "==> Skipping launch (--no-launch)"
    exit 0
fi

echo ""
echo "==> Launching..."
echo "    Tip: env vars skip keychain. Run './scripts/gateway.sh --export-env' for setup."
exec "$DEST"
