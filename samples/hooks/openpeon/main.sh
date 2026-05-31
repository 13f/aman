#!/bin/bash
# openpeon — 根据 Aman hook 事件随机播放 openpeon 包中的 wav 音效
# 读取 ~/.openpeon/packs/<pack>/openpeon.json，按事件类别随机选一个音效播放
set -euo pipefail

INPUT=$(cat)
EVENT_TYPE=$(echo "$INPUT" | jq -r '.event_type // empty' 2>/dev/null || exit 0)
[ -z "$EVENT_TYPE" ] && exit 0
echo "[openpeon] event: $EVENT_TYPE" >&2

# 事件 → openpeon 类别映射
case "$EVENT_TYPE" in
  session:started|gateway:ready)   CATEGORY="session.start" ;;
  agent:busy|llm:call_started)     CATEGORY="task.acknowledge" ;;
  tool:completed|message:completed|llm:call_ended|session:closed)
                                   CATEGORY="task.complete" ;;
  tool:failed|agent:reply_stream_error)
                                   CATEGORY="task.error" ;;
  *)                               exit 0 ;;
esac

# pack 优先级：环境变量 > config.yaml > 默认
CONFIG_DIR="$(cd "$(dirname "$0")" && pwd)"
CONFIG_PACK=$(grep '^pack:' "$CONFIG_DIR/config.yaml" 2>/dev/null | awk '{print $2}' || true)
PACK="${OPENPEON_PACK:-${CONFIG_PACK:-peon}}"
OPENPEON_DIR="${OPENPEON_DIR:-$HOME/.openpeon}"
PACK_FILE="$OPENPEON_DIR/packs/$PACK/openpeon.json"

[ -f "$PACK_FILE" ] || exit 0

# 从 JSON 中提取该类别的 sound 列表，随机选一个
SOUND=$(jq -r --arg cat "$CATEGORY" '
  .categories[$cat].sounds // [] | .[].file
' "$PACK_FILE" | sort -R | head -1)

[ -z "$SOUND" ] && exit 0

WAV_PATH="$OPENPEON_DIR/packs/$PACK/$SOUND"
[ -f "$WAV_PATH" ] || exit 0

# 播放（macOS 用 afplay，Linux 用 aplay）
case "$(uname -s)" in
  Darwin) afplay "$WAV_PATH" ;;
  Linux)  aplay "$WAV_PATH" >/dev/null 2>&1 || true ;;
esac
