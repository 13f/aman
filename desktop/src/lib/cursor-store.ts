// ---------------------------------------------------------------------------
// Cursor store — custom cursor state (imperative, DOM-driven)
// ---------------------------------------------------------------------------
//
// Drives the custom cursor system. Cursor classes are applied to
// `document.body`, so CSS can react with `cursor: url(...)` or custom
// cursor elements.
//
// Emotion data arrives via SSE events from the gateway; the emotion→cursor
// mapping translates the agent's current emotional state into a visual
// cursor treatment.

export type CursorMode =
  | "normal"       // thin line cursor (default)
  | "thinking"     // agent is processing / ReAct reasoning — pulsing ring
  | "grab"         // draggable region hover
  | "grabbing"     // actively dragging
  | "excited"      // emotion: golden glow
  | "reflective";  // emotion: soft blue-purple

// ---------------------------------------------------------------------------
// Emotion → cursor mapping
// ---------------------------------------------------------------------------
// Maps emotion IDs (as defined in the agent's emotions/data.json) to
// cursor modes. Add entries here as the emotion vocabulary grows.

const EMOTION_TO_CURSOR: Record<string, CursorMode> = {
  excited: "excited",
  happy: "excited",
  reflective: "reflective",
  calm: "reflective",
  thinking: "thinking",
  focused: "thinking",
  curious: "reflective",
};

// Patterns of activity that imply "thinking" regardless of emotion label
const THINKING_EMOTIONS = new Set(["thinking", "focused", "processing", "working"]);

// ---------------------------------------------------------------------------
// Module state (plain vars — updated imperatively, read by applyCursorClass)
// ---------------------------------------------------------------------------

let currentMode: CursorMode = "normal";
let emotionOverride: CursorMode | null = null;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/** Current cursor mode. */
export function getCursorMode(): CursorMode {
  return emotionOverride ?? currentMode;
}

/** Set cursor directly (e.g. from streaming/processing state). */
export function setCursorMode(mode: CursorMode): void {
  currentMode = mode;
  applyCursorClass();
}

/** Set cursor from an emotion ID string (from SSE or state). */
export function setCursorFromEmotion(emotionId: string): void {
  const lower = emotionId.toLowerCase();
  const mapped = EMOTION_TO_CURSOR[lower];
  if (mapped) {
    emotionOverride = mapped;
  }
  // If a "thinking" emotion arrives, also update currentMode so the
  // pulsing-ring takes effect even without an explicit setCursorMode call.
  if (THINKING_EMOTIONS.has(lower)) {
    currentMode = "thinking";
  }
  applyCursorClass();
}

/** Clear emotion override and revert to the base mode. */
export function clearEmotionOverride(): void {
  emotionOverride = null;
  applyCursorClass();
}

/** Set grab/grabbing for drag interactions. */
export function setGrab(active: boolean): void {
  currentMode = active ? "grabbing" : "grab";
  applyCursorClass();
}

/** Reset to normal cursor. */
export function resetCursor(): void {
  currentMode = "normal";
  emotionOverride = null;
  applyCursorClass();
}

// ---------------------------------------------------------------------------
// DOM application
// ---------------------------------------------------------------------------

const MODE_CLASSES: Record<CursorMode, string> = {
  normal: "cursor-normal",
  thinking: "cursor-thinking",
  grab: "cursor-grab",
  grabbing: "cursor-grabbing",
  excited: "cursor-excited",
  reflective: "cursor-reflective",
};

function applyCursorClass(): void {
  if (typeof document === "undefined") return;
  const mode = emotionOverride ?? currentMode;
  // Remove all cursor classes
  for (const cls of Object.values(MODE_CLASSES)) {
    document.body.classList.remove(cls);
  }
  // Add the current one
  const cls = MODE_CLASSES[mode];
  if (cls) {
    document.body.classList.add(cls);
  }
}
