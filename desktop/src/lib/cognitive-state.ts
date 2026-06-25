// Cognitive State — types, phase inference, and step text derivation
// for the CognitiveRing component (Level 1 of the Cognitive State Map feature).
//
// Pure functions — NO Svelte runes or mutable state.
// Callers (Home.svelte, ActivityStateWidget.svelte) maintain their own
// per-agent CognitiveState records and call these functions from event handlers.

export type ReactPhase = "observing" | "thinking" | "acting" | "result" | "idle";

export interface CognitiveState {
  phase: ReactPhase;
  currentStep: string;
}

/** Per-phase segment colors (match the design system accent palette). */
export const PHASE_COLORS: Record<ReactPhase, string> = {
  observing: "#60A5FA", // blue
  thinking:  "#F59E0B", // amber
  acting:    "#22D3EE", // cyan
  result:    "#A78BFA", // purple
  idle:      "#6c8cff", // fallback blue (IdleRing default)
};

// ── Phase state machine ──────────────────────────────────────────────

/**
 * Derive the next ReAct phase from an incoming event.
 *
 * State transitions:
 *   idle      + processing event       → observing
 *   observing + reply_stream_start     → thinking
 *   thinking  + tool:dispatched        → acting
 *   acting    + tool:completed/failed  → result
 *   thinking  + reply_ready (no tools) → result  (LLM replied directly)
 *   result    + reply_stream_start     → thinking (next cycle)
 *   result    + tool:dispatched        → acting   (next cycle, fast)
 *   *         + idle event             → idle     (reset)
 */
export function inferReactPhase(
  eventType: string,
  _data: unknown,
  prev: CognitiveState,
): ReactPhase {
  // Reset on idle
  if (eventType === "idle") return "idle";

  // Processing events
  const isProcessing =
    eventType === "agent:reply_stream_start" ||
    eventType === "agent:reply_chunk" ||
    eventType === "agent:reply_stream_done" ||
    eventType === "agent:reply_ready" ||
    eventType === "tool:dispatched" ||
    eventType === "tool:completed" ||
    eventType === "tool:failed";

  if (!isProcessing) return prev.phase;

  // From idle → start observing
  if (prev.phase === "idle") return "observing";

  // stream_start / chunk → thinking (LLM is generating)
  if (
    eventType === "agent:reply_stream_start" ||
    eventType === "agent:reply_chunk"
  ) {
    return "thinking";
  }

  // Tool dispatched → acting
  if (eventType === "tool:dispatched") return "acting";

  // Tool completed / failed → result
  if (eventType === "tool:completed" || eventType === "tool:failed") {
    return "result";
  }

  // reply_ready (final reply, LLM responded without tool calls) → result
  if (eventType === "agent:reply_ready") return "result";

  return prev.phase;
}

// ── Step text ────────────────────────────────────────────────────────

/** Human-friendly labels for well-known tool names. */
const TOOL_STEP_LABELS: Record<string, string> = {
  search: "searching…",
  web_search: "searching the web…",
  web_fetch: "fetching page…",
  read_file: "reading file…",
  write_file: "writing file…",
  bash: "running command…",
  grep: "searching code…",
  edit: "editing content…",
  ask: "asking for input…",
  computer: "interacting…",
  text_editor: "editing text…",
  glob: "finding files…",
  list_files: "listing files…",
  read: "reading…",
  write: "writing…",
  execute: "running…",
};

/**
 * Derive a human-readable step description from an incoming event.
 */
export function inferStepText(
  eventType: string,
  data: any,
  _prev: CognitiveState,
): string {
  // Tool dispatched — use tool name for a specific label
  if (eventType === "tool:dispatched") {
    const toolName: string | undefined = data?.tool_name;
    if (toolName) {
      return TOOL_STEP_LABELS[toolName] ?? `running ${toolName}…`;
    }
    return "using tools…";
  }

  // Stream events
  if (
    eventType === "agent:reply_stream_start" ||
    eventType === "agent:reply_chunk"
  ) {
    return "thinking…";
  }

  if (eventType === "agent:reply_stream_done") {
    return "processing…";
  }

  if (eventType === "tool:completed") {
    return "evaluating result…";
  }

  if (eventType === "tool:failed") {
    return "handling error…";
  }

  if (eventType === "agent:reply_ready") {
    return "responding…";
  }

  return "";
}

/** Returns true when the caller should start a 1.5 s auto-observe timer. */
export function isResultTransition(
  phase: ReactPhase,
  prevPhase: ReactPhase,
): boolean {
  return phase === "result" && prevPhase !== "result";
}

/** Returns true when this event should finalise the cycle → idle. */
export function isFinalReply(
  eventType: string,
  phase: ReactPhase,
): boolean {
  return eventType === "agent:reply_ready" && phase === "result";
}
