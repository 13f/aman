// agent-context — latest assembled context snapshot per agent.
//
// Populated by AgentWindow when it receives `agent:context_ready` events,
// consumed by the Context tab. The snapshot rides along with the normal
// message flow (backend publishes it when a turn's context is assembled) —
// no polling, no extra update mechanism.

export interface ContextSnapshot {
  agent_id: string;
  session_id: string;
  /** True when derived from the session's persisted event stream rather than a
   * live `agent:context_ready` snapshot (sessions that predate the feature). */
  reconstructed?: boolean;
  system: { name: string; snippet: string; tokens: number };
  tools: Array<{ name: string; description: string }>;
  tools_tokens: number;
  memory: Array<{ content: string; importance: number; timestamp: string | null }>;
  memory_tokens: number;
  conversation: {
    count: number;
    tokens: number;
    /** User↔assistant dialogue. Present on both live snapshots (backend emits
     * it) and rebuilt ones — the Context tab shows the same shape either way. */
    messages?: Array<{ role: "user" | "assistant"; content: string }>;
  };
  grounding: { knowledge: string; situation: string };
  token_total: number;
  token_max: number;
  usage_percent: number;
  updated_at: string;
}

// Reactive module state (Svelte 5 runes) — mutating `snapshots[key]` in the
// event listener triggers re-render in whatever component reads it.
const snapshots: Record<string, ContextSnapshot> = $state({});

export function setAgentContext(agentKey: string, snapshot: ContextSnapshot) {
  snapshots[agentKey] = { ...snapshot, updated_at: new Date().toLocaleTimeString() };
}

export function clearAgentContext(agentKey: string) {
  delete snapshots[agentKey];
}

export function getAgentContext(agentKey: string): ContextSnapshot | undefined {
  return snapshots[agentKey];
}

/**
 * Rebuild a best-effort context snapshot for a session whose events were
 * persisted before `agent:context_ready` existed (older sessions have no
 * snapshot to restore). Derives what the session actually contained — its
 * user↔assistant dialogue and the tools it called — from the persisted JSONL
 * event stream. Pure read of already-persisted data; no backend call, no
 * extra update mechanism. Returns null when the session has no usable content.
 */
export function rebuildContextFromSession(
  agentKey: string,
  sessionId: string,
  events: Array<{ event_id?: string; event_type: string; payload?: any }>,
): ContextSnapshot | null {
  const conversation: Array<{ role: "user" | "assistant"; content: string }> = [];
  const tools = new Map<string, string>(); // tool name -> description (unknown for old sessions)
  let tokenTotal = 0;
  // Same event_id can be appended twice to the JSONL (pre-existing persistence
  // quirk) — dedup like the chat history loader does, or the dialogue repeats.
  const seen = new Set<string>();

  const pushText = (role: "user" | "assistant", text: string) => {
    const trimmed = text.trim();
    if (!trimmed) return;
    conversation.push({ role, content: trimmed });
    tokenTotal += Math.max(1, Math.ceil(trimmed.length / 4));
  };

  for (const evt of events) {
    if (evt.event_id && seen.has(evt.event_id)) continue;
    if (evt.event_id) seen.add(evt.event_id);
    const et = evt.event_type ?? "";
    const p = evt.payload ?? {};
    if (et === "MessageReceived") {
      pushText("user", String(p.text ?? ""));
    } else if ((et.includes("reply_ready") || et === "llm_reply_ready") && typeof p.reply === "string") {
      pushText("assistant", p.reply);
    } else if (
      (et.includes("tool:dispatched") || et.includes("tool:completed") || et.includes("tool:failed"))
      && p.tool_name
    ) {
      tools.set(String(p.tool_name), "");
    } else if (et.includes("agent:got_tool_calls") && Array.isArray(p.tools)) {
      for (const t of p.tools) {
        tools.set(String(t?.tool_name ?? t?.name ?? "tool"), "");
      }
    }
  }

  if (conversation.length === 0 && tools.size === 0) return null;

  return {
    agent_id: agentKey,
    session_id: sessionId,
    reconstructed: true,
    system: { name: agentKey, snippet: "", tokens: 0 },
    tools: Array.from(tools, ([name]) => ({ name, description: "" })),
    tools_tokens: 0,
    memory: [],
    memory_tokens: 0,
    conversation: { count: conversation.length, tokens: tokenTotal, messages: conversation },
    grounding: { knowledge: "unknown", situation: "unknown" },
    token_total: tokenTotal,
    token_max: 0,
    usage_percent: 0,
  };
}
