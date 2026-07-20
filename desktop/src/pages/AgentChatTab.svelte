<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { renderMarkdown } from "../lib/markdown";
  import ToolCallCard from "./ToolCallCard.svelte";
  import type { ToolCallData } from "./ToolCallCard.svelte";
  import { t, locale } from "../lib/i18n.svelte";
  import {
    dayKey,
    formatMessageTime,
    formatMessageDateLabel,
    formatMessageFull,
  } from "../lib/format-time";

  const { agentKey }: { agentKey: string } = $props();

  // ── Types (shared with Chat.svelte) ────────────────────────────────────
  type MessageType =
    | "user_text" | "user_command"
    | "assistant_text" | "assistant_streaming"
    | "assistant_tool_call" | "assistant_tool_result"
    | "system_event" | "security_alert";
  type MessageStatus = "pending" | "sent" | "streaming" | "completed" | "interrupted" | "error";

  interface Message {
    id: string;
    type: MessageType;
    content: string;
    timestamp: string;
    sessionId: string;
    status: MessageStatus;
    toolCall?: ToolCallData;
    traceId?: string;
    archived?: boolean;
  }

  interface Session {
    id: string;
    title: string;
    messageCount: number;
    status: "idle" | "processing";
    createdAt?: number;
    lastActiveAt?: number;
    state?: string;
  }

  // ── State ──────────────────────────────────────────────────────────────
  let sessions = $state<Session[]>([]);
  let activeSessionId = $state("");
  let messages = $state<Message[]>([]);
  let inputText = $state("");
  let isLoading = $state(false);
  let activeStreamingMessageId: string | null = null;
  let streamingContent = "";
  let messageAreaEl: HTMLDivElement | undefined = $state();
  let chatInputRef: HTMLElement | null = $state(null);
  let titleInputRef: HTMLInputElement | null = $state(null);
  let autoScroll = $state(true);
  let sessionsLoaded = $state(false);
  let toasts = $state<Array<{ id: string; type: "info"|"warn"|"error"|"success"; message: string; timeout: ReturnType<typeof setTimeout>|null }>>([]);
  let stopWindowTimer: ReturnType<typeof setTimeout> | null = null;
  let sessionsPerPage = 10;
  let currentPage = $state(1);
  let deletingSessionId = $state<string | null>(null);
  // Title edit state for the active session header (click → input, Enter → save).
  let editingTitle = $state(false);
  let editTitleValue = $state("");

  const paginatedSessions = $derived(
    sessions.slice((currentPage - 1) * sessionsPerPage, currentPage * sessionsPerPage)
  );
  const totalPages = $derived(Math.max(1, Math.ceil(sessions.length / sessionsPerPage)));
  const activeMessages = $derived(messages.filter(m => m.sessionId === activeSessionId));
  const isProcessing = $derived(
    isLoading || messages.some(m => m.sessionId === activeSessionId && (m.status === "pending" || m.status === "streaming"))
  );

  // ── Date-divided message list ──────────────────────────────────────
  // Each entry is either a date divider or a message, so history reads like
  // a real chat Timeline ("Today / Yesterday / Jul 19").
  type RenderItem =
    | { kind: "divider"; key: string; label: string; }
    | { kind: "msg"; message: Message };

  const renderItems = $derived.by(() => {
    const tag = locale().code;
    const items: RenderItem[] = [];
    let lastDay = "";
    for (const message of activeMessages) {
      const day = dayKey(message.timestamp, tag);
      if (day !== lastDay) {
        items.push({ kind: "divider", key: `d-${message.id}`, label: formatMessageDateLabel(message.timestamp, tag) });
        lastDay = day;
      }
      items.push({ kind: "msg", message });
    }
    return items;
  });

  let unlisteners: (() => void) = [];

  // ── Helpers ────────────────────────────────────────────────────────────
  function showToast(type: "info"|"warn"|"error"|"success", message: string, durationMs = 5000) {
    const id = crypto.randomUUID();
    const toast = { id, type, message, timeout: null as ReturnType<typeof setTimeout>|null };
    toast.timeout = setTimeout(() => {
      toasts = toasts.filter(t => t.id !== id);
    }, durationMs);
    toasts = [...toasts, toast];
  }

  function updateMessage(id: string, patch: Partial<Message>) {
    messages = messages.map(m => (m.id === id ? { ...m, ...patch } : m));
  }

  function updateSession(id: string, patch: Partial<Session>) {
    sessions = sessions.map(s => (s.id === id ? { ...s, ...patch } : s));
  }

  // Update a single session's status ("idle" | "processing"). Used by the
  // send/stop pipeline to reflect the agent's current activity in the session
  // list.
  function updateSessionStatus(id: string, status: "idle" | "processing") {
    sessions = sessions.map(s => (s.id === id ? { ...s, status } : s));
  }

  function escapeMarkdown(text: string): string {
    const lines = text.split("\n");
    const GFM_SEP = /^\|[\s\-:|]+\|$/;
    const PIPE_ROW = /^\|.+\|$/;
    let inTableBody = false;
    const out: string[] = [];
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      let processed = line;
      if (/^-{3,}$/.test(processed.trim())) {
        processed = processed.replace(/^(-{3,})$/, "\\$1");
        inTableBody = false;
      } else if (/^={4,}$/.test(processed.trim())) {
        processed = processed.replace(/^(={4,})$/, "\\$1");
        inTableBody = false;
      } else if (/^={4,}[^=].*$/.test(processed.trim())) {
        processed = "\\" + processed;
        inTableBody = false;
      } else if (GFM_SEP.test(processed.trim())) {
        inTableBody = true;
      } else if (PIPE_ROW.test(processed.trim())) {
        if (inTableBody) {
          // leave as-is
        } else if (i + 1 < lines.length && GFM_SEP.test(lines[i + 1].trim())) {
          // valid table header
        } else {
          const pipeCount = (processed.match(/\|/g) || []).length;
          if (pipeCount >= 3) processed = "`" + processed + "`";
        }
      } else if (processed.trim().length > 0) {
        inTableBody = false;
      }
      out.push(processed);
    }
    return out.join("\n");
  }

  function safeMarkdownHtml(node: HTMLElement, content: string) {
    node.innerHTML = renderMarkdown(escapeMarkdown(content));
    return { update(newContent: string) { node.innerHTML = renderMarkdown(escapeMarkdown(newContent)); } };
  }

  // ── Session loading ────────────────────────────────────────────────────
  async function loadSessions() {
    try {
      const list = await invoke<Array<{
        id: string; state: string; message_count: number;
        created_at: number; last_active_at: number | null;
        title?: string;
      }>>("chat_session_list_db", { agentKey });
      const loaded: Session[] = list.map((s, i) => ({
        id: s.id,
        title: s.title || (s.id.length > 8 ? `Session ${s.id.slice(0, 8)}` : `Session ${i + 1}`),
        messageCount: s.message_count,
        status: "idle" as const,
        createdAt: s.created_at,
        lastActiveAt: s.last_active_at ?? s.created_at,
        state: s.state,
      }));
      sessions = loaded;
      sessionsLoaded = true;
      if (activeSessionId && !loaded.some(s => s.id === activeSessionId)) {
        activeSessionId = "";
      }
    } catch {
      try {
        const list = await invoke<Array<{
          id: string; state: string; message_count: number;
          created_at: number; last_active_at: number | null;
          title?: string;
        }>>("chat_session_list", { agentKey });
        list.sort((a, b) => (b.last_active_at ?? b.created_at) - (a.last_active_at ?? a.created_at));
        sessions = list.map((s, i) => ({
          id: s.id,
          title: s.title || (s.id.length > 8 ? `Session ${s.id.slice(0, 8)}` : `Session ${i + 1}`),
          messageCount: s.message_count,
          status: "idle" as const,
          createdAt: s.created_at,
          lastActiveAt: s.last_active_at ?? s.created_at,
          state: s.state,
        }));
      } catch { /* no source */ }
      sessionsLoaded = true;
    }
  }

  function updateSessionTitleFromMessages(sessionId: string) {
    const session = sessions.find(s => s.id === sessionId);
    if (!session || (!session.title.startsWith("Chat ") && !session.title.startsWith("Session "))) return;
    const firstUserMsg = messages.find(m => m.sessionId === sessionId && m.type === "user_text");
    if (firstUserMsg) {
      const text = firstUserMsg.content.trim();
      const title = text.length <= 40 ? text : text.slice(0, 40) + '…';
      sessions = sessions.map(s => s.id === sessionId ? { ...s, title } : s);
      invoke("chat_session_rename", { agentKey, sessionId, title }).catch(() => {});
    }
  }

  // ── Manual title rename (drives the click-to-edit header) ───────────────
  async function renameActiveSession(title: string) {
    if (!activeSessionId) return;
    const trimmed = title.trim();
    if (!trimmed) { editingTitle = false; return; }
    sessions = sessions.map(s => s.id === activeSessionId ? { ...s, title: trimmed } : s);
    editingTitle = false;
    try { await invoke("chat_session_rename", { agentKey, sessionId: activeSessionId, title: trimmed }); } catch { /* non-fatal */ }
  }
  function startEditTitle() {
    const cur = sessions.find(s => s.id === activeSessionId);
    if (!cur) return;
    editTitleValue = cur.title;
    editingTitle = true;
  }

  // Focus the rename input once it mounts (replaces `autofocus` for a11y).
  $effect(() => {
    if (editingTitle) titleInputRef?.focus();
  });

  // ── Session history ─────────────────────────────────────────────────────
  async function loadSessionHistory(sessionId: string) {
    try {
      const state = await invoke<{
        session_id: string;
        messages: Array<{ event_id: string; event_type: string; payload: any; timestamp_ms: number }>;
      }>("chat_session_state_local", { agentKey, sessionId });
      if (!state.messages?.length) return;
      const historyMsgs: Message[] = [];
      const seenIds = new Set(messages.map(m => m.id));
      const toolResults: Array<{ callId: string; success: boolean; output: any }> = [];
      for (const evt of state.messages) {
        const et = evt.event_type;
        const p = evt.payload ?? {};
        if (seenIds.has(evt.event_id)) continue;
        seenIds.add(evt.event_id);
        let msg: Message | null = null;
        if (et === "MessageReceived") {
          msg = { id: evt.event_id, type: "user_text", content: p.text ?? "", timestamp: new Date(evt.timestamp_ms).toISOString(), sessionId, status: "completed" };
        } else if (et.includes("reply_ready") || et.includes("reply_stream_done") || et === "llm_reply_ready") {
          const replyText = p.reply ?? p.full_text ?? "";
          if (replyText) msg = { id: evt.event_id, type: "assistant_text", content: replyText, timestamp: new Date(evt.timestamp_ms).toISOString(), sessionId, status: "completed" };
        } else if (et === "agent:got_tool_calls" || et.includes("tool:dispatched") || et === "llm_tool_call" || et.includes("tool_call")) {
          // agent:got_tool_calls: {tools: ["web_search", "write"], turn: 2}
          // tool:dispatched / llm_tool_call: {tool_call_id, tool_name, args}
          if (Array.isArray(p.tools)) {
            for (const toolName of p.tools) {
              const callId = `${evt.event_id}:${toolName}`;
              historyMsgs.push({ id: callId, type: "assistant_tool_call", content: `Tool: ${toolName}`, timestamp: new Date(evt.timestamp_ms).toISOString(), sessionId, status: "streaming", toolCall: { callId, toolName, arguments: "{}", status: "running" as const } });
            }
          } else {
            const callId: string = p.tool_call_id ?? p.call_id ?? evt.event_id;
            const toolName: string = p.tool_name ?? p.name ?? "tool";
            msg = { id: callId, type: "assistant_tool_call", content: `Tool: ${toolName}`, timestamp: new Date(evt.timestamp_ms).toISOString(), sessionId, status: "streaming", toolCall: { callId, toolName, arguments: typeof p.args === "string" ? p.args : JSON.stringify(p.args ?? {}), status: "running" as const } };
          }
        } else if (et.includes("tool:completed") || et.includes("tool:failed")) {
          const callId: string = p.tool_call_id ?? p.call_id ?? "";
          const success = et.includes("tool:completed") || p.success === true;
          toolResults.push({ callId, success, output: p.output ?? p.result, toolName: p.tool_name });
        } else if (et === "history_trimmed" || et.includes("config_warning")) {
          msg = { id: evt.event_id, type: "system_event", content: p.message ?? "", timestamp: new Date(evt.timestamp_ms).toISOString(), sessionId, status: "completed" };
        }
        if (msg) historyMsgs.push(msg);
      }
      for (const tr of toolResults) {
        const callMsg = historyMsgs.find(m => m.type === "assistant_tool_call" && (m.toolCall?.callId === tr.callId || (tr.callId === "" && m.toolCall?.toolName === tr.toolName) || m.id === tr.callId));
        if (callMsg && callMsg.toolCall && callMsg.toolCall.status === "running") {
          callMsg.toolCall.status = tr.success ? "success" : "failed";
          callMsg.status = tr.success ? "completed" : "error";
          if (tr.success) callMsg.toolCall.result = tr.output; else callMsg.toolCall.error = tr.output;
        }
      }
      for (const msg of historyMsgs) {
        if (msg.type === "assistant_tool_call" && msg.toolCall?.status === "running") {
          msg.toolCall.status = "success";
          msg.status = "completed";
        }
      }
      if (historyMsgs.length > 0) messages = [...messages, ...historyMsgs];
    } catch { /* gateway unavailable */ }
  }

  function selectSession(id: string) {
    activeSessionId = id;
    const count = messages.filter(m => m.sessionId === id).length;
    updateSession(id, { messageCount: count });
    loadSessionHistory(id);
  }

  async function createSession() {
    let id: string;
    try {
      id = await invoke<string>("chat_session_create", { agentKey });
    } catch {
      id = Array.from({ length: 12 }, () => Math.floor(Math.random() * 16).toString(16)).join('');
    }
    const count = sessions.length + 1;
    sessions = [{ id, title: `Chat ${count}`, messageCount: 0, status: "idle", createdAt: Date.now() }, ...sessions];
    activeSessionId = id;
    currentPage = 1;
  }

  async function deleteSession(id: string) {
    const session = sessions.find(s => s.id === id);
    if (!session) return;
    try {
      await invoke("chat_session_delete", { sessionId: id });
      sessions = sessions.filter(s => s.id !== id);
      messages = messages.filter(m => m.sessionId !== id);
      if (activeSessionId === id) {
        const next = sessions.length > 0 ? sessions[Math.min(currentPage - 1, sessions.length - 1)] : null;
        if (next) activeSessionId = next.id; else await createSession();
      }
      if (paginatedSessions.length === 0 && currentPage > 1) currentPage--;
      showToast("success", t("chat.session_deleted"));
    } catch (e) {
      showToast("error", t("chat.failed_delete_session").replace("{e}", String(e)));
    }
    deletingSessionId = null;
  }

  // ── Sending / Stopping ─────────────────────────────────────────────────
  async function stopGeneration() {
    const streamingId = activeStreamingMessageId;
    if (stopWindowTimer) clearTimeout(stopWindowTimer);
    stopWindowTimer = setTimeout(() => {
      const msg = streamingId ? messages.find(m => m.id === streamingId) : null;
      if (msg && msg.status === "streaming") {
        updateMessage(streamingId, { status: "interrupted" });
        activeStreamingMessageId = null;
      }
      stopWindowTimer = null;
    }, 500);
    try { await invoke("chat_stop_generation", { sessionId: activeSessionId }); } catch { /* non-fatal */ }
    isLoading = false;
    updateSessionStatus(activeSessionId, "idle");
    inputText = "";
  }

  async function sendMessage(text?: string) {
    text = (text ?? inputText).trim();
    if (!text) return;
    if (text === "/stop") { await stopGeneration(); return; }
    inputText = "";
    if (chatInputRef) (chatInputRef as any).value = "";
    const tempId = crypto.randomUUID();
    const userMsg: Message = { id: tempId, type: "user_text", content: text, timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "pending" };
    messages = [...messages, userMsg];
    isLoading = true;
    updateSessionStatus(activeSessionId, "processing");
    try {
      const eventId = await invoke<string>("chat_send_message", { text, sessionId: activeSessionId });
      messages = messages.map(m => (m.id === tempId ? { ...m, id: eventId, status: "sent" } : m));
    } catch (err: any) {
      const errStr = typeof err === "string" ? err : (err?.message ?? String(err));
      updateMessage(tempId, { status: "error", content: `Error: ${errStr}` });
      isLoading = false;
      updateSessionStatus(activeSessionId, "idle");
    }
  }

  // ── Scroll ─────────────────────────────────────────────────────────────
  function handleScroll() {
    if (!messageAreaEl) return;
    const el = messageAreaEl;
    autoScroll = el.scrollHeight - el.scrollTop - el.clientHeight <= 60;
  }

  function scrollToBottom() {
    requestAnimationFrame(() => { if (messageAreaEl) messageAreaEl.scrollTop = messageAreaEl.scrollHeight; });
  }

  $effect(() => {
    if (messages.length > 0 && autoScroll) scrollToBottom();
  });

  // ── Event handlers ─────────────────────────────────────────────────────
  function handleAgentStreamStart(data: any) {
    const sid: string = data.session_id;
    const msgId = crypto.randomUUID();
    activeStreamingMessageId = msgId;
    streamingContent = "";
    messages = [...messages, { id: msgId, type: "assistant_streaming", content: "", timestamp: new Date().toISOString(), sessionId: sid, status: "streaming" }];
  }

  function handleAgentChunk(data: any) {
    const delta: string = data.extra?.delta ?? "";
    if (!delta || !activeStreamingMessageId) return;
    streamingContent += delta;
    updateMessage(activeStreamingMessageId, { content: streamingContent });
  }

  function handleAgentStreamDone(data: any) {
    if (activeStreamingMessageId) {
      if (streamingContent) {
        updateMessage(activeStreamingMessageId, { type: "assistant_text", status: "completed" });
      } else {
        messages = messages.filter(m => m.id !== activeStreamingMessageId);
      }
      activeStreamingMessageId = null;
    }
    streamingContent = "";
    const finishReason: string = data.extra?.finish_reason ?? "";
    if (finishReason !== "tool_calls") {
      isLoading = false;
      updateSessionStatus(data.session_id, "idle");
    }
    updateSessionTitleFromMessages(data.session_id);
  }

  function handleAgentReplyReady(data: any) {
    const sid: string = data.session_id;
    const streamingMsg = messages.find(m => m.sessionId === sid && m.type === "assistant_streaming");
    if (streamingMsg) {
      updateMessage(streamingMsg.id, { type: "assistant_text", content: data.reply, status: "completed" });
      if (activeStreamingMessageId === streamingMsg.id) activeStreamingMessageId = null;
    } else {
      messages = [...messages, { id: crypto.randomUUID(), type: "assistant_text", content: data.reply, timestamp: new Date().toISOString(), sessionId: sid, status: "completed" }];
    }
    isLoading = false;
    updateSessionStatus(sid, "idle");
    updateSessionTitleFromMessages(sid);
  }

  function handleAgentToolCall(data: any) {
    const callId: string = data.tool_call_id;
    messages = [...messages, { id: callId, type: "assistant_tool_call", content: `Tool: ${data.tool_name}`, timestamp: new Date().toISOString(), sessionId: data.session_id, status: "streaming", toolCall: { callId, toolName: data.tool_name, arguments: JSON.stringify(data.args ?? {}), status: "running" } }];
  }

  function handleAgentToolResult(data: any) {
    const callId: string = data.tool_call_id;
    const msg = messages.find(m => m.id === callId);
    if (!msg || msg.type !== "assistant_tool_call") return;
    const newStatus = data.success === true ? "success" : "failed";
    messages = messages.map(m => m.id === callId ? { ...m, toolCall: { callId, toolName: msg.toolCall?.toolName ?? "unknown", arguments: msg.toolCall?.arguments ?? "{}", status: newStatus, result: data.output, error: data.success === false ? data.output : undefined }, status: newStatus === "success" ? "completed" : "error" } : m);
  }

  function handleAgentStreamError(data: any) {
    const sid: string = data.session_id;
    if (activeStreamingMessageId) { updateMessage(activeStreamingMessageId, { status: "error" }); activeStreamingMessageId = null; }
    isLoading = false;
    updateSessionStatus(sid, "idle");
  }

  let agentHarnessSessions = $state(new Set<string>());

  async function handleEventProcessed(e: any) {
    const payload = e.payload;
    if (!payload) return;

    // The SSE bridge emits the whole `kernel::Event` as the Tauri payload:
    //   { id, source, event_type, payload: { agent_id, session_id, ... }, ... }
    // So the *real* business data lives one level deeper — `payload.payload`.
    // (ActivityStateWidget already reads this way; the previous code read the
    // top-level fields and always saw `undefined`, silently dropping events.)
    const eventType: string = payload.event_type;
    const data = payload.payload ?? {};

    // Scope to this window's agent. The agent_id lives in the inner payload,
    // or nested again at data.payload.agent_id for events that re-wrap it.
    const eventAgentId: string | undefined =
      data.agent_id ?? data.payload?.agent_id;
    if (eventAgentId && eventAgentId !== agentKey) return;

    // Background idle-run session events — ignore (handled in Home tab)
    if (eventType === "MessageReceived" && data?.background === true) return;

    // The first event of a brand-new session (MessageReceived) may carry only
    // `agent_id` and no `session_id` until the store has allocated one. Do not
    // drop it — call sites below are tolerant of a missing session_id where it
    // makes sense, and dropping it here was the root cause of "chat sends but
    // nothing happens".
    if (!data?.session_id && eventType === "MessageReceived") {
      // fall through — handled below
    } else if (!data?.session_id) {
      return;
    }

    switch (eventType) {
      case "agent:reply_stream_start":
        handleAgentStreamStart(data); break;
      case "agent:reply_chunk":
        handleAgentChunk(data); break;
      case "agent:reply_stream_done":
        handleAgentStreamDone(data); break;
      case "agent:reply_stream_error":
        handleAgentStreamError(data); break;
      case "agent:reply_ready":
      case "agent:reply_interrupted":
        handleAgentReplyReady(data); break;
      case "tool:dispatched":
        handleAgentToolCall(data); break;
      case "tool:completed":
      case "tool:failed":
        handleAgentToolResult(data); break;
    }
  }

  // ── chat-input web component wiring ────────────────────────────────────
  $effect(() => {
    const el = chatInputRef;
    if (!el) return;
    function onSend(e: Event) { sendMessage((e as CustomEvent).detail?.text || ""); }
    function onStop() { stopGeneration(); }
    function onInput(e: Event) { inputText = (e as CustomEvent).detail?.text || ""; }
    el.addEventListener("send", onSend);
    el.addEventListener("stop", onStop);
    el.addEventListener("input", onInput);
    return () => {
      el.removeEventListener("send", onSend);
      el.removeEventListener("stop", onStop);
      el.removeEventListener("input", onInput);
    };
  });

  onMount(async () => {
    const unlisten = await listen("event:processed", handleEventProcessed);
    unlisteners.push(unlisten);

    // Listen for prefill events (fired by main app when an agent window is
    // opened via a skill card). Creates a fresh session and populates input.
    const unlistenPrefill = await listen<{ agentKey: string; text: string }>(
      "agent-window:prefill",
      async (event) => {
        if (event.payload.agentKey !== agentKey) return;
        await createSession();
        inputText = event.payload.text;
        if (chatInputRef) (chatInputRef as any).value = event.payload.text;
        // Switch parent to chat tab.
        window.dispatchEvent(new CustomEvent("agent-window:switch-tab", { detail: "chat" }));
      },
    );
    unlisteners.push(unlistenPrefill);

    await loadSessions();
    if (sessions.length > 0) {
      selectSession(sessions[0].id);
    }
  });

  onDestroy(() => {
    for (const u of unlisteners) u();
    unlisteners = [];
    if (stopWindowTimer) clearTimeout(stopWindowTimer);
  });
</script>

<div class="chat-tab">
  <!-- Sessions panel (left) -->
  <aside class="sessions-panel">
    <div class="sessions-header">
      <span>Sessions</span>
      <button class="new-session-btn" onclick={createSession} title={t("chat.new_chat_title")}>+</button>
    </div>
    <div class="sessions-list">
      {#each paginatedSessions as session (session.id)}
        <div
          class="session-item"
          class:active={session.id === activeSessionId}
          class:processing={session.status === "processing"}
          role="button"
          tabindex="0"
          onclick={() => selectSession(session.id)}
          onkeydown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); selectSession(session.id); } }}
        >
          <span class="session-title">{session.title}</span>
          <span class="session-meta">{session.messageCount}m</span>
          {#if session.id === activeSessionId}
            <button class="session-del" onclick={(e) => { e.stopPropagation(); deletingSessionId = session.id; }} title="Delete">×</button>
          {/if}
        </div>
      {/each}
    </div>
    {#if totalPages > 1}
      <div class="sessions-pagination">
        <button disabled={currentPage <= 1} onclick={() => currentPage--}>‹</button>
        <span>{currentPage}/{totalPages}</span>
        <button disabled={currentPage >= totalPages} onclick={() => currentPage++}>›</button>
      </div>
    {/if}
  </aside>

  <!-- Chat area (right) -->
  <div class="chat-area">
    {#if !activeSessionId}
      <div class="chat-empty">
        <p>{t("chat.select_session_hint")}</p>
        <button class="btn-primary" onclick={createSession}>{t("chat.new_chat")}</button>
      </div>
    {:else}
      <div class="chat-header">
        {#if editingTitle}
          <input
            class="chat-title-input"
            type="text"
            bind:value={editTitleValue}
            onkeydown={(e) => {
              if (e.key === 'Enter') { e.preventDefault(); void renameActiveSession(editTitleValue); }
              else if (e.key === 'Escape') { editingTitle = false; }
            }}
            onblur={() => void renameActiveSession(editTitleValue)}
            bind:this={titleInputRef}
          />
        {:else}
          <span class="chat-title" title={"Click to rename"} role="button" tabindex="0" onclick={startEditTitle} onkeydown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); startEditTitle(); } }}>{sessions.find(s => s.id === activeSessionId)?.title ?? "Chat"}</span>
        {/if}
        {#if isProcessing}
          <button class="btn-stop" onclick={stopGeneration}>{t("chat.stop")}</button>
        {/if}
      </div>

      <div class="messages" bind:this={messageAreaEl} onscroll={handleScroll}>
        {#each renderItems as item (item.kind === "divider" ? item.key : item.message.id)}
          {#if item.kind === "divider"}
            <div class="date-divider"><span>{item.label}</span></div>
          {:else}
            {@const message = item.message}
            {@const isUser = message.type === "user_text"}
            {@const isAssistant = message.type.startsWith("assistant")}
            {@const isSystem = message.type === "system_event"}
            {@const timeLabel = formatMessageTime(message.timestamp, locale().code)}
            <div class="msg" class:user={isUser} class:assistant={isAssistant} class:system={isSystem}>
              {#if !isSystem}
                <div class="msg-meta">
                  <span class="msg-role">{isUser ? t("chat.role_user") : t("chat.role_assistant")}</span>
                  <span class="msg-time" title={formatMessageFull(message.timestamp, locale().code)}>{timeLabel}</span>
                </div>
              {/if}
              {#if message.type === "assistant_tool_call" && message.toolCall}
                <ToolCallCard data={message.toolCall} />
              {:else if message.content}
                <div class="msg-body" use:safeMarkdownHtml={message.content}></div>
              {/if}
            </div>
          {/if}
        {/each}
      </div>

      <!-- Input area -->
      <div class="chat-input-zone">
        <chat-input
          bind:this={chatInputRef}
          placeholder={t("chat.message_placeholder")}
          buttontext={t("chat.send")}
          stoptext={t("chat.stop")}
          disabled={isProcessing || !activeSessionId}
          processing={isProcessing ? "" : undefined}
        ></chat-input>
      </div>
    {/if}
  </div>
</div>

<!-- Delete confirmation dialog -->
{#if deletingSessionId}
  <div class="dialog-overlay" role="button" tabindex="-1" aria-label={t("common.cancel")} onclick={(e) => { if (e.target === e.currentTarget) deletingSessionId = null; }} onkeydown={(e) => { if (e.key === "Escape" || e.key === "Enter" || e.key === " ") deletingSessionId = null; }}>
    <div class="dialog" role="dialog" aria-modal="true">
      <p>{t("chat.delete_session_confirm")}</p>
      <div class="dialog-actions">
        <button class="btn-cancel" onclick={() => deletingSessionId = null}>{t("common.cancel")}</button>
        <button class="btn-danger" onclick={() => deleteSession(deletingSessionId!)}>{t("chat.delete")}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .chat-tab {
    display: flex;
    height: 100%;
    min-height: 0;
  }

  /* ── Sessions panel ─────────────────────────────────────────────── */
  .sessions-panel {
    width: 200px;
    min-width: 160px;
    border-right: 1px solid var(--border, rgba(255, 255, 255, 0.06));
    display: flex;
    flex-direction: column;
    background: color-mix(in srgb, var(--bg, #0b1313) 50%, transparent);
  }

  .sessions-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 12px;
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--fg-dim, #9ca3af);
    border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.06));
  }

  .new-session-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    font-size: 18px;
    line-height: 1;
    padding: 0;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.1));
    border-radius: 6px;
    background: transparent;
    color: var(--fg-dim, #9ca3af);
    cursor: pointer;
  }

  .new-session-btn:hover { color: var(--fg, #e5e7eb); border-color: var(--fg-dim); }

  .sessions-list {
    flex: 1;
    overflow-y: auto;
    padding: 4px;
  }

  .session-item {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 8px 10px;
    font-size: 13px;
    text-align: left;
    background: transparent;
    border: none;
    border-radius: 8px;
    color: var(--fg-dim, #9ca3af);
    cursor: pointer;
    transition: background 0.12s;
  }

  .session-item:hover { background: color-mix(in srgb, var(--fg, #e5e7eb) 6%, transparent); }
  .session-item.active { background: color-mix(in srgb, var(--accent, #6366f1) 14%, transparent); color: var(--fg, #e5e7eb); }
  .session-item.processing .session-title::after { content: " ●"; color: var(--green, #22c55e); }

  .session-title { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .session-meta { font-size: 11px; opacity: 0.6; }

  .session-del {
    font-size: 14px;
    line-height: 1;
    background: transparent;
    border: none;
    color: var(--fg-dim, #9ca3af);
    cursor: pointer;
    padding: 0 4px;
    border-radius: 4px;
  }
  .session-del:hover { color: #ef4444; background: rgba(239, 68, 68, 0.12); }

  .sessions-pagination {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 8px;
    font-size: 12px;
    color: var(--fg-dim, #9ca3af);
    border-top: 1px solid var(--border, rgba(255, 255, 255, 0.06));
  }

  .sessions-pagination button {
    background: transparent;
    border: 1px solid var(--border, rgba(255, 255, 255, 0.1));
    border-radius: 4px;
    color: var(--fg-dim, #9ca3af);
    cursor: pointer;
    padding: 2px 8px;
  }
  .sessions-pagination button:disabled { opacity: 0.3; cursor: default; }

  /* ── Chat area ──────────────────────────────────────────────────── */
  .chat-area {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .chat-empty {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 16px;
    color: var(--fg-dim, #9ca3af);
  }

  .btn-primary {
    padding: 10px 24px;
    background: var(--accent, #6366f1);
    color: #fff;
    border: none;
    border-radius: 8px;
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
  }

  .chat-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 20px;
    border-bottom: 1px solid var(--border, rgba(255, 255, 255, 0.06));
    flex-shrink: 0;
  }

  .chat-title {
    font-size: 14px;
    font-weight: 600;
    color: var(--fg, #e5e7eb);
    cursor: pointer;
    border-radius: 4px;
    padding: 2px 6px;
    margin: -2px -6px;
  }
  .chat-title:hover { background: rgba(255, 255, 255, 0.05); }
  .chat-title-input {
    font-size: 14px;
    font-weight: 600;
    color: var(--fg, #e5e7eb);
    background: transparent;
    border: 1px solid var(--accent, #5b73f5);
    border-radius: 4px;
    padding: 2px 6px;
    margin: -2px -6px;
    width: 100%;
    max-width: 280px;
    outline: none;
  }

  .btn-stop {
    padding: 4px 14px;
    font-size: 12px;
    border-radius: 6px;
    border: 1px solid var(--yellow, #f0a020);
    color: var(--yellow, #f0a020);
    background: transparent;
    cursor: pointer;
  }
  .btn-stop:hover { background: color-mix(in srgb, var(--yellow, #f0a020) 12%, transparent); }

  .messages {
    flex: 1;
    overflow-y: auto;
    padding: 16px 20px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .msg {
    max-width: 85%;
    padding: 10px 14px;
    border-radius: 12px;
    font-size: 14px;
    line-height: 1.5;
    color: var(--fg, #e5e7eb);
  }

  .msg.user {
    align-self: flex-end;
    background: color-mix(in srgb, var(--accent, #6366f1) 20%, transparent);
    border: 1px solid color-mix(in srgb, var(--accent, #6366f1) 30%, transparent);
  }

  .msg.assistant {
    align-self: flex-start;
    background: var(--bg-card, rgba(255, 255, 255, 0.03));
    border: 1px solid var(--border, rgba(255, 255, 255, 0.06));
  }

  .msg.system {
    align-self: center;
    max-width: 95%;
    font-size: 12px;
    color: var(--fg-dim, #9ca3af);
    background: color-mix(in srgb, var(--fg-dim) 8%, transparent);
    border: 1px solid var(--border, rgba(255, 255, 255, 0.04));
  }

  .msg-body { word-break: break-word; }

  .msg-meta {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 4px;
    font-size: 11px;
    color: var(--fg-dim, #9ca3af);
  }

  .msg-role { font-weight: 600; }

  .msg.user .msg-meta { justify-content: flex-end; }

  .msg-time { opacity: 0.7; font-variant-numeric: tabular-nums; }

  .date-divider {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 14px 0 6px;
    color: var(--fg-dim, #9ca3af);
    font-size: 11px;
  }
  .date-divider::before,
  .date-divider::after {
    content: "";
    flex: 1;
    height: 1px;
    background: var(--border, rgba(255, 255, 255, 0.06));
  }
  .date-divider span {
    padding: 2px 10px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--fg-dim) 8%, transparent);
    white-space: nowrap;
  }

  .chat-input-zone {
    padding: 12px 16px;
    border-top: 1px solid var(--border, rgba(255, 255, 255, 0.06));
    flex-shrink: 0;
  }

  /* ── Dialog ─────────────────────────────────────────────────────── */
  .dialog-overlay {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.6);
  }

  .dialog {
    padding: 24px;
    background: var(--bg-card, #1a1d2e);
    border: 1px solid var(--border, rgba(255, 255, 255, 0.1));
    border-radius: 12px;
    min-width: 280px;
    text-align: center;
  }

  .dialog-actions {
    display: flex;
    gap: 12px;
    justify-content: center;
    margin-top: 16px;
  }

  .btn-cancel {
    padding: 8px 20px;
    background: transparent;
    border: 1px solid var(--border);
    color: var(--fg-dim, #9ca3af);
    border-radius: 8px;
    cursor: pointer;
  }

  .btn-danger {
    padding: 8px 20px;
    background: #ef4444;
    color: #fff;
    border: none;
    border-radius: 8px;
    cursor: pointer;
  }
</style>
