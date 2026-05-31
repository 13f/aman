<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";
  import ToolCallCard from "./ToolCallCard.svelte";
  import type { ToolCallData } from "./ToolCallCard.svelte";
  import { marked } from "marked";

  marked.setOptions({ gfm: true, breaks: true });

  let { prefillInput = "", prefillSeq = 0 }: { prefillInput?: string; prefillSeq?: number } = $props();

  // Escape standalone markdown structural elements that would eat content:
  // - standalone `---` rendered as invisible <hr>
  // - standalone `===` that can act as setext heading underlines
  // - malformed table rows (pipe-delimited without proper separator) that
  //   marked/gfm tries to parse as tables but renders as garbled text
  // This preserves inline formatting (bold, code, lists, tables) while
  // preventing accidental content loss from LLM-generated section separators.
  function escapeMarkdown(text: string): string {
    return text
      // Escape standalone --- lines (horizontal rules)
      .replace(/^---+$/gm, "\\---")
      // Escape standalone === lines (setext headings or HR-like)
      .replace(/^====+$/gm, "\\===")
      // Escape lines that start with ==== and have content (like ASCII separator headers)
      .replace(/^(====+)([^=].*)$/gm, "\\$1$2")
      // Escape malformed table rows: lines starting with | that don't form
      // a proper GFM table (missing separator row after header).
      // Wrap them in backtick-escaping to prevent marked from interpreting them.
      .replace(/^(\|[^\n]+\|)$/gm, (_, line: string) => {
        // Only escape if it looks like a malformed table (no separator row context)
        // Count the pipes — if more than 2, likely a table row
        const pipeCount = (line.match(/\|/g) || []).length;
        return pipeCount >= 3 ? "`" + line + "`" : line;
      });
  }

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
    channelType?: string;
    archived?: boolean;
    traceId?: string;
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

  let sessions = $state<Session[]>([]);
  let activeSessionId = $state("");
  let messages = $state<Message[]>([]);
  let inputText = $state("");
  let isLoading = $state(false);
  let currentSoulName = $state("");
  let rateLimitCountdown = $state(0);
  let unlisteners: (() => void)[] = [];
  let messageAreaEl: HTMLDivElement | undefined = $state();
  let chatInputRef: HTMLElement | null = $state(null);
  let autoScroll = $state(true);
  let chatCapabilityAvailable = $state(true);
  let soulDescription = $state("");
  let soulDetailExpanded = $state(false);
  let soulIntroShown = $state(false);
  let archivedMsgIds = $state<Set<string>>(new Set());
  let toasts = $state<Array<{ id: string; type: "info" | "warn" | "error" | "success"; message: string; timeout: ReturnType<typeof setTimeout> | null }>>([]);

  // Skill picker state
  let skillList = $state<Array<{ name: string; description: string }>>([]);
  let showSkillPicker = $state(false);
  let skillPickerResults = $state<Array<{ name: string; description: string }>>([]);
  let skillPickerIndex = $state(0);
  // Built-in command names that should NOT trigger the skill picker
  const BUILTIN_COMMAND_NAMES = ["help", "h", "?", "session", "retry", "r", "stop", "edit", "e", "export", "trace", "tc", "soul"];

  // Pagination
  let currentPage = $state(1);
  let sessionsPerPage = 10;
  let sessionsLoaded = $state(false);
  let isExploring = $state(false);
  let idleRunningTag = $state<string | null>(null);
  let dailyLifeOpen = $state(false);
  let deletingSessionId = $state<string | null>(null);
  let idleAvailability = $state<Record<string, { work: boolean; study: boolean; fun: boolean }>>({});
  // Track background idle-run sessions for toast-only notifications
  let backgroundIdleSessions = $state<Set<string>>(new Set());
  let backgroundSessionTags = $state<Map<string, string>>(new Map());
  // Track sessions that have a detached process running (awaiting detach completion)
  let awaitingDetachSessions = $state<Set<string>>(new Set());

  const paginatedSessions = $derived(
    sessions.slice((currentPage - 1) * sessionsPerPage, currentPage * sessionsPerPage)
  );
  const totalPages = $derived(Math.max(1, Math.ceil(sessions.length / sessionsPerPage)));

  const activeSession = $derived(sessions.find(s => s.id === activeSessionId));
  const activeMessages = $derived(messages.filter(m => m.sessionId === activeSessionId));
  const isProcessing = $derived(
    isLoading || messages.some(m => m.sessionId === activeSessionId && (m.status === "pending" || m.status === "streaming"))
  );

  // Agent selector state
  let agentList = $state<Array<{ key: string; display_name: string; provider: string }>>([]);
  let activeAgentKey = $state("");
  let editingTitle = $state(false);
  let editTitleValue = $state("");

  const activeAgentHasProvider = $derived(
    agentList.find(a => a.key === activeAgentKey)?.provider != null &&
    agentList.find(a => a.key === activeAgentKey)?.provider !== ""
  );

  async function loadAgents() {
    try {
      const agents = await invoke<Array<{ key: string; display_name: string; provider: string; is_active: boolean }>>("list_agents");
      agentList = agents;
      // Prefer active agent that has a provider configured
      const active = agents.find(a => a.is_active && a.provider);
      if (active) {
        activeAgentKey = active.key;
        // Fresh entry into Chat — load sessions for the active agent.
        await loadSessions();
      } else if (agents.length > 0) {
        // Auto-select first agent that has a provider, or first agent
        const firstConfigured = agents.find(a => a.provider);
        activeAgentKey = firstConfigured ? firstConfigured.key : agents[0].key;
        handleAgentChange();
      }
    } catch (e) {
      showToast("error", `Failed to load agents: ${e}`);
    }
  }

  async function loadIdleAvailability() {
    try {
      const result = await invoke<{ agents: Record<string, { work: boolean; study: boolean; fun: boolean }> }>("list_idle_availability");
      idleAvailability = result.agents || {};
    } catch {
      // silently keep defaults — buttons stay enabled, backend errors surface on click
    }
  }

  async function handleAgentChange() {
    if (!activeAgentKey) return;
    try {
      await invoke("select_agent", { key: activeAgentKey });
      showToast("info", `Switched to agent: ${activeAgentKey}`);
      activeSessionId = "";
      messages = [];
      await loadSessions();
      await loadIdleAvailability();
      if (sessions.length > 0) {
        selectSession(sessions[0].id);
      }
    } catch (e) {
      showToast("error", `Failed to select agent: ${e}`);
    }
  }

  async function handleAgentSelect(key: string) {
    if (key === activeAgentKey) return;
    activeAgentKey = key;
    try {
      await invoke("select_agent", { key });
      showToast("info", `Switched to agent: ${key}`);
      activeSessionId = "";
      messages = [];
      await loadSessions();
      if (sessions.length > 0) {
        selectSession(sessions[0].id);
      }
    } catch (e) {
      showToast("error", `Failed to select agent: ${e}`);
    }
  }

  // ── Skill picker ─────────────────────────────────────────────────────

  async function loadSkills() {
    try {
      const v = await invoke<any>("list_llm_skills");
      const items = v?.items as Array<{ name: string; description: string }> | undefined;
      skillList = (items || []).filter(s => s.name && s.description);
    } catch {
      // Skills unavailable — picker stays empty
    }
  }

  function updateSkillPicker() {
    const text = inputText;
    // Only trigger on "/skill" command (not any "/")
    if (!text.startsWith("/skill")) {
      showSkillPicker = false;
      return;
    }

    // Extract the part after "/skill" (the skill name prefix)
    const afterCommand = text.slice("/skill".length);
    // If user has typed something after "/skill" with a space, they are entering args
    if (afterCommand.startsWith(" ")) {
      showSkillPicker = false;
      return;
    }

    const prefix = afterCommand.trim().toLowerCase();

    // Filter skills by prefix
    if (prefix) {
      skillPickerResults = skillList.filter(
        s => s.name.toLowerCase().includes(prefix) ||
             s.description.toLowerCase().includes(prefix)
      );
    } else {
      skillPickerResults = [...skillList];
    }

    showSkillPicker = skillPickerResults.length > 0;
    skillPickerIndex = 0;
  }

  function applySkillPickerSelection(skillName: string) {
    inputText = "/skill " + skillName + " ";
    showSkillPicker = false;
    if (chatInputRef) {
      chatInputRef.value = inputText;
      chatInputRef.focus();
    }
  }

  function closeSkillPicker() {
    showSkillPicker = false;
  }

  function updateMessage(id: string, patch: Partial<Message>) {
    messages = messages.map(m => (m.id === id ? { ...m, ...patch } : m));
  }

  function updateSession(id: string, patch: Partial<Session>) {
    sessions = sessions.map(s => (s.id === id ? { ...s, ...patch } : s));
  }

  async function saveTitle() {
    editingTitle = false;
    const newTitle = editTitleValue.trim();
    if (!activeSession || !activeSessionId) return;

    // Update local state immediately
    updateSession(activeSessionId, { title: newTitle || undefined });

    // Persist to backend
    try {
      await invoke("chat_session_rename", {
        agentKey: activeAgentKey || null,
        sessionId: activeSessionId,
        title: newTitle,
      });
    } catch (e) {
      console.error("Failed to persist session title:", e);
    }
  }

  function updateSessionStatus(id: string, status: "idle" | "processing") {
    updateSession(id, { status });
  }

  function selectSession(id: string) {
    activeSessionId = id;
    const count = messages.filter(m => m.sessionId === id).length;
    updateSession(id, { messageCount: count });
    // Load session history from the backend.
    loadSessionHistory(id);
  }

  async function loadSessionHistory(sessionId: string) {
    try {
      const state = await invoke<{
        session_id: string;
        messages: Array<{
          event_id: string;
          event_type: string;
          payload: any;
          timestamp_ms: number;
        }>;
      }>("chat_session_state_local", { agentKey: activeAgentKey, sessionId });
      if (!state.messages?.length) return;
      // Build Message objects from persisted session events.
      // JSONL stores event_type as Rust Debug format, e.g.
      //   MessageReceived, Custom("tool:dispatched"), Custom("agent:reply_ready")
      const historyMsgs: Message[] = [];
      const seenIds = new Set(messages.map(m => m.id));
      // Two-pass: collect tool:dispatched then apply tool:completed/failed results.
      const toolResults: Array<{ callId: string; success: boolean; output: any }> = [];
      for (const evt of state.messages) {
        const et = evt.event_type;
        const p = evt.payload ?? {};
        if (seenIds.has(evt.event_id)) continue;
        seenIds.add(evt.event_id);
        let msg: Message | null = null;
        if (et === "MessageReceived") {
          // User message
          msg = {
            id: evt.event_id,
            type: "user_text",
            content: p.text ?? "",
            timestamp: new Date(evt.timestamp_ms).toISOString(),
            sessionId,
            status: "completed",
          };
        } else if (et.includes("reply_ready") || et.includes("reply_stream_done") || et === "llm_reply_ready") {
          // Assistant reply (exclude stream-start artifacts)
          const replyText = p.reply ?? p.full_text ?? "";
          if (replyText) {
            msg = {
              id: evt.event_id,
              type: "assistant_text",
              content: replyText,
              timestamp: new Date(evt.timestamp_ms).toISOString(),
              sessionId,
              status: "completed",
            };
          }
        } else if (et.includes("tool:dispatched") || et === "llm_tool_call" || et.includes("tool_call")) {
          // Tool invocation — stored as Custom("tool:dispatched") in JSONL.
          const callId: string = p.tool_call_id ?? p.call_id ?? evt.event_id;
          const toolName: string = p.tool_name ?? p.name ?? "tool";
          msg = {
            id: callId,
            type: "assistant_tool_call",
            content: `Tool: ${toolName}`,
            timestamp: new Date(evt.timestamp_ms).toISOString(),
            sessionId,
            status: "streaming",
            toolCall: {
              callId,
              toolName,
              arguments: typeof p.args === "string" ? p.args : JSON.stringify(p.args ?? {}),
              status: "running" as const,
            },
          };
        } else if (et.includes("tool:completed") || et.includes("tool:failed")) {
          // Stored as Custom("tool:completed") or Custom("tool:failed").
          const callId: string = p.tool_call_id ?? p.call_id ?? "";
          const success = et.includes("tool:completed") || p.success === true;
          toolResults.push({ callId, success, output: p.output ?? p.result });
        } else if (et === "history_trimmed" || et === "HISTORY_TRIMMED" || et.includes("config_warning")) {
          // System event
          msg = {
            id: evt.event_id,
            type: "system_event",
            content: p.message ?? "",
            timestamp: new Date(evt.timestamp_ms).toISOString(),
            sessionId,
            status: "completed",
          };
        }
        if (msg) {
          historyMsgs.push(msg);
        }
      }
      // Second pass: apply tool:completed/failed results to matching tool_call messages.
      for (const tr of toolResults) {
        const callMsg = historyMsgs.find(m => m.type === "assistant_tool_call" && m.toolCall?.callId === tr.callId);
        if (callMsg && callMsg.toolCall) {
          callMsg.toolCall.status = tr.success ? "success" : "failed";
          callMsg.status = tr.success ? "completed" : "error";
          if (tr.success) {
            callMsg.toolCall.result = tr.output;
          } else {
            callMsg.toolCall.error = tr.output;
          }
        }
      }
      // Any tool call still "running" after history replay is stale —
      // mark it completed so the send button isn't stuck as "Stop".
      for (const msg of historyMsgs) {
        if (msg.type === "assistant_tool_call" && msg.toolCall?.status === "running") {
          msg.toolCall.status = "success";
          msg.status = "completed";
        }
      }
      if (historyMsgs.length > 0) {
        messages = [...messages, ...historyMsgs];
      }
    } catch {
      // Gateway not available or session not found — no history to load.
    }
  }

  async function loadSessions() {
    try {
      // Try reading from the local SQLite DB for the active agent.
      const list = await invoke<Array<{
        id: string; state: string; message_count: number;
        created_at: number; last_active_at: number | null;
        session_type: string | null; title?: string;
      }>>("chat_session_list_db", { agentKey: activeAgentKey || null });
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
      // Keep current active session if still in the list
      if (activeSessionId && !loaded.some(s => s.id === activeSessionId)) {
        activeSessionId = "";
      }
    } catch (_dbErr) {
      // DB doesn't exist or no agents configured — fall back to gateway API.
      try {
        const list = await invoke<Array<{
          id: string; state: string; message_count: number;
          created_at: number; last_active_at: number | null;
          session_type: string | null; title?: string; agent_id?: string;
        }>>("chat_session_list", { agentKey: activeAgentKey || null });
        list.sort((a, b) => (b.last_active_at ?? b.created_at) - (a.last_active_at ?? a.created_at));
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
      } catch (_gatewayErr) {
        console.warn("No session source available");
      }
      sessionsLoaded = true;
    }
  }

  function updateSessionTitleFromMessages(sessionId: string) {
    const session = sessions.find(s => s.id === sessionId);
    // Only update if the title is still the default placeholder.
    if (!session || !session.title.startsWith("Chat ") && !session.title.startsWith("Session ")) return;
    const firstUserMsg = messages.find(m => m.sessionId === sessionId && m.type === "user_text");
    if (firstUserMsg) {
      const text = firstUserMsg.content.trim();
      const title = text.length <= 40 ? text : text.slice(0, 40) + '…';
      sessions = sessions.map(s => s.id === sessionId ? { ...s, title } : s);
    }
  }

  async function createSession() {
    let id: string;
    try {
      id = await invoke<string>("chat_session_create", { agentKey: activeAgentKey || null });
    } catch {
      // Runtime not running — create a local-only session
      // Short 12-char hex ID similar to xid format
      id = Array.from({ length: 12 }, () => Math.floor(Math.random() * 16).toString(16)).join('');
    }
    const count = sessions.length + 1;
    sessions = [{ id, title: `Chat ${count}`, messageCount: 0, status: "idle", createdAt: Date.now() }, ...sessions];
    activeSessionId = id;
    currentPage = 1;
  }

  async function startExplore() {
    if (isExploring) return;
    isExploring = true;
    try {
      const result = await invoke<{ session_id: string; source: string }>("explore_start", {
        agentKey: activeAgentKey || null,
      });
      const count = sessions.length + 1;
      sessions = [{ id: result.session_id, title: `Explore ${count}`, messageCount: 0, status: "idle", createdAt: Date.now() }, ...sessions];
      activeSessionId = result.session_id;
      currentPage = 1;
    } catch (e) {
      showToast("error", `Explore failed: ${e}`);
    } finally {
      isExploring = false;
    }
  }

  async function startIdleRun(tag: string) {
    if (idleRunningTag) return;
    idleRunningTag = tag;
    try {
      const result = await invoke<{ session_id: string; skill_name: string; tag: string }>("idle_run", {
        tag,
        agentKey: activeAgentKey || null,
        background: true,
      });
      const label = tag.charAt(0).toUpperCase() + tag.slice(1);
      // Track as background session — don't switch activeSessionId or create local session
      backgroundIdleSessions.add(result.session_id);
      backgroundSessionTags.set(result.session_id, label);
      showToast("info", `DailyLife ${label} started`, 4000);
    } catch (e) {
      const err = String(e);
      if (err.includes("执行失败")) {
        showToast("error", "执行失败，还没有实装有关的技能");
      } else {
        showToast("error", `${tag} run failed: ${err}`);
      }
      idleRunningTag = null;
    }
  }

  async function deleteSession(id: string) {
    const session = sessions.find(s => s.id === id);
    if (!session) return;
    try {
      await invoke("chat_session_delete", { sessionId: id });
      sessions = sessions.filter(s => s.id !== id);
      messages = messages.filter(m => m.sessionId !== id);
      if (activeSessionId === id) {
        // Select the next available session, or create one
        const next = sessions.length > 0 ? sessions[Math.min(currentPage - 1, sessions.length - 1)] : null;
        if (next) {
          activeSessionId = next.id;
        } else {
          await createSession();
        }
      }
      // Adjust page if current page is now empty
      if (paginatedSessions.length === 0 && currentPage > 1) {
        currentPage = currentPage - 1;
      }
      showToast("success", "Session deleted");
    } catch (e) {
      showToast("error", `Failed to delete session: ${e}`);
    }
    deletingSessionId = null;
  }

  function handleScroll() {
    if (!messageAreaEl) return;
    const el = messageAreaEl;
    const threshold = 60;
    autoScroll = el.scrollHeight - el.scrollTop - el.clientHeight <= threshold;
  }

  function scrollToBottom() {
    requestAnimationFrame(() => {
      if (messageAreaEl) {
        messageAreaEl.scrollTop = messageAreaEl.scrollHeight;
      }
    });
  }

  // Auto-scroll on new messages
  $effect(() => {
    if (messages.length > 0 && autoScroll) {
      scrollToBottom();
    }
  });

  function showToast(type: "info" | "warn" | "error" | "success", message: string, durationMs = 5000) {
    const id = crypto.randomUUID();
    const toast = { id, type, message, timeout: null as ReturnType<typeof setTimeout> | null };
    toast.timeout = setTimeout(() => {
      toasts = toasts.filter(t => t.id !== id);
    }, durationMs);
    toasts = [...toasts, toast];
  }

  function dismissToast(id: string) {
    const t = toasts.find(t => t.id === id);
    if (t?.timeout) clearTimeout(t.timeout);
    toasts = toasts.filter(t => t.id !== id);
  }

  // Load SOUL info on mount
  async function loadSoulInfo() {
    try {
      const info = await invoke<{ current_soul: string | null; last_changed: string | null }>("get_soul_info");
      if (info.current_soul) {
        currentSoulName = info.current_soul;
      }
    } catch { /* runtime not ready yet */ }
  }

  // Loading timeout — force reset if no events arrive
  $effect(() => {
    if (isLoading) {
      const timeout = setTimeout(() => { isLoading = false; }, 300000);
      return () => clearTimeout(timeout);
    }
  });

  // Rate limit countdown timer
  $effect(() => {
    if (rateLimitCountdown > 0) {
      const interval = setInterval(() => {
        rateLimitCountdown = Math.max(0, rateLimitCountdown - 1);
      }, 1000);
      return () => clearInterval(interval);
    }
  });

  function handleMessageQueued(data: any) {
    const msgId: string = data.message_id;
    updateMessage(msgId, { status: "sent" });
  }

  function handleMessageDropped(data: any) {
    const droppedId: string = data.dropped_message_id;
    updateMessage(droppedId, { status: "error", content: `Message dropped: ${data.reason ?? "queue full"}` });
    isLoading = false;
    updateSessionStatus(data.session_id, "idle");
  }

  function handleLlmReplyReady(data: any) {
    const channelType: string | undefined = data.channel_type;
    const traceId: string | undefined = data.trace_id;
    const reply: Message = {
      id: crypto.randomUUID(),
      type: "assistant_text",
      content: data.reply,
      timestamp: new Date().toISOString(),
      sessionId: data.session_id,
      status: "completed",
      channelType,
      traceId,
    };
    messages = [...messages, reply];
    updateMessage(data.original_message_id, { status: "completed", channelType });
    isLoading = false;
    updateSessionStatus(data.session_id, "idle");
    updateSessionTitleFromMessages(data.session_id);
    if (data.soul_name) {
      currentSoulName = data.soul_name;
      // Show SOUL intro on first reply
      if (!soulIntroShown) {
        soulIntroShown = true;
        messages = [...messages, {
          id: crypto.randomUUID(),
          type: "system_event",
          content: `You are talking to **${data.soul_name}**. Ask me anything!`,
          timestamp: new Date().toISOString(),
          sessionId: data.session_id,
          status: "completed" as MessageStatus,
        }];
      }
    }
  }

	  function handleLlmToolCall(data: any) {
	    // data = { session_id, call_id, tool_name, arguments }
	    const callId: string = data.call_id;
	    const channelType: string | undefined = data.channel_type;
	    const traceId: string | undefined = data.trace_id;
	    const toolCall: Message = {
	      id: callId,
	      type: "assistant_tool_call",
	      content: `Tool: ${data.tool_name}`,
	      timestamp: new Date().toISOString(),
	      sessionId: data.session_id,
	      status: "streaming",
	      channelType,
	      traceId,
	      toolCall: {
	        callId,
	        toolName: data.tool_name,
	        arguments: data.arguments ?? "{}",
	        status: "running",
	      },
	    };
	    messages = [...messages, toolCall];

	  }

  function handleLlmToolResult(data: any) {
    // data = { session_id, call_id, status, result?, error? }
    const callId: string = data.call_id;
    const msg = messages.find(m => m.id === callId);
    if (!msg || msg.type !== "assistant_tool_call") return;

    const newStatus: "success" | "failed" = data.status === "success" ? "success" : "failed";
    const updatedToolCall: ToolCallData = {
      callId,
      toolName: msg.toolCall?.toolName ?? "unknown",
      arguments: msg.toolCall?.arguments ?? "{}",
      status: newStatus,
      result: data.result,
      error: data.error,
    };
    messages = messages.map(m =>
      m.id === callId
        ? { ...m, toolCall: updatedToolCall, status: newStatus === "success" ? "completed" : "error" }
        : m,
    );

  }

  // ── AgentHarness event handlers (Phase B migration) ──
  // Local accumulator avoids stale state reads when chunks arrive in rapid succession.
  let streamingContent = "";
  let agentHarnessSessions = $state(new Set<string>());

  function handleAgentStreamStart(data: any) {
    const sid: string = data.session_id;
    const msgId = crypto.randomUUID();
    activeStreamingMessageId = msgId;
    streamingContent = "";
    const streamMsg: Message = {
      id: msgId,
      type: "assistant_streaming",
      content: "",
      timestamp: new Date().toISOString(),
      sessionId: sid,
      status: "streaming",
    };
    messages = [...messages, streamMsg];
  }

  function handleAgentChunk(data: any) {
    const delta: string = data.extra?.delta ?? "";
    if (!delta || !activeStreamingMessageId) return;
    streamingContent += delta;
    updateMessage(activeStreamingMessageId, { content: streamingContent });
  }

  function handleAgentStreamDone(data: any) {
    if (activeStreamingMessageId) {
      // Log raw content before markdown rendering for debugging truncation
      const rawPreview = streamingContent.length > 200
        ? streamingContent.slice(0, 200) + "..."
        : streamingContent;
      console.log("[Chat] stream done, raw start:", rawPreview, "length:", streamingContent.length);
      if (streamingContent) {
        // Stream produced content → keep the message
        updateMessage(activeStreamingMessageId, { type: "assistant_text", status: "completed" });
      } else {
        // Empty stream (tool-only ReAct turn) → remove the bubble
        messages = messages.filter(m => m.id !== activeStreamingMessageId);
      }
      activeStreamingMessageId = null;
    }
    streamingContent = "";
    agentHarnessSessions = new Set([...agentHarnessSessions, data.session_id]);
    isLoading = false;
    updateSessionStatus(data.session_id, "idle");
    updateSessionTitleFromMessages(data.session_id);
  }

  function handleAgentReplyReady(data: any) {
    const sid: string = data.session_id;
    // Clean up detach tracking if present
    if (awaitingDetachSessions.has(sid)) {
      awaitingDetachSessions = new Set([...awaitingDetachSessions].filter(s => s !== sid));
    }
    // Dedup: if streaming already delivered the reply, skip the fallback
    if (agentHarnessSessions.has(sid)) return;
    // Find existing streaming message for this session and replace its content.
    const streamingMsg = messages.find(m => m.sessionId === sid && m.type === "assistant_streaming");
    if (streamingMsg) {
      updateMessage(streamingMsg.id, {
        type: "assistant_text",
        content: data.reply,
        status: "completed",
      });
      if (activeStreamingMessageId === streamingMsg.id) {
        activeStreamingMessageId = null;
      }
    } else {
      const reply: Message = {
        id: crypto.randomUUID(),
        type: "assistant_text",
        content: data.reply,
        timestamp: new Date().toISOString(),
        sessionId: sid,
        status: "completed",
      };
      messages = [...messages, reply];
    }
    isLoading = false;
    updateSessionStatus(sid, "idle");
    updateSessionTitleFromMessages(sid);
  }

  function handleAgentToolCall(data: any) {
    // data = { agent_id, session_id, tool_call_id, tool_name, args }
    const callId: string = data.tool_call_id;
    const toolCall: Message = {
      id: callId,
      type: "assistant_tool_call",
      content: `Tool: ${data.tool_name}`,
      timestamp: new Date().toISOString(),
      sessionId: data.session_id,
      status: "streaming",
      toolCall: {
        callId,
        toolName: data.tool_name,
        arguments: JSON.stringify(data.args ?? {}),
        status: "running",
      },
    };
    messages = [...messages, toolCall];

  }

  function handleAgentToolResult(data: any) {
    // Map AgentHarness fields to what handleLlmToolResult expects
    handleLlmToolResult({
      session_id: data.session_id,
      call_id: data.tool_call_id,
      status: data.success === true ? "success" : "failed",
      result: data.output,
      error: data.success === false ? data.output : undefined,
    });
  }

  function handleAgentStreamError(data: any) {
    const sid: string = data.session_id;
    if (awaitingDetachSessions.has(sid)) {
      awaitingDetachSessions = new Set([...awaitingDetachSessions].filter(s => s !== sid));
    }
    if (activeStreamingMessageId) {
      updateMessage(activeStreamingMessageId, { status: "error" });
      activeStreamingMessageId = null;
    }
    isLoading = false;
    updateSessionStatus(sid, "idle");
  }

  function handleAgentHistoryCompressed(data: any) {
    handleHistoryTrimmed({
      session_id: data.session_id,
      removed: data.messages_removed ?? 0,
      remaining: data.remaining_messages ?? 0,
      tokens_saved: data.tokens_saved ?? 0,
      usage_pct: data.token_usage_pct ?? 0,
    });
  }

  async function handleEventProcessed(e: any) {
    const payload = e.payload;
    const eventType: string = payload.event_type;
    const data = payload.payload;
    console.log("[Chat] event:", eventType, "sid:", data?.session_id, "active:", activeSessionId);
    if (eventType === "llm_reply_ready") console.log("[Chat] reply data:", JSON.stringify(data));

    // Capability events use different payload structure
    if (eventType === "capability_removed") {
      // Individual event: { capability: "chat", plugin: "gateway" }
      const cap: string = data?.capability ?? "";
      if (cap === "chat") {
        chatCapabilityAvailable = false;
        showToast("warn", `Chat capability removed (plugin: ${data?.plugin ?? "unknown"})`);
        // Phase 4.5: close active tabs, clear message buffer
        // but DON'T clear persistent state (history can be restored later)
        messages = messages.filter(m => m.sessionId !== activeSessionId);
        sessions = sessions.filter(s => s.status === "idle");
        // Pick the first remaining session
        if (sessions.length > 0) {
          activeSessionId = sessions[0].id;
        } else {
          activeSessionId = "";
        }
      } else {
        showToast("info", `Capability removed: ${cap}`);
      }
      return;
    }
    if (eventType === "capability_available") {
      const cap: string = data?.capability ?? "";
      if (cap === "chat") {
        chatCapabilityAvailable = true;
        showToast("success", "Chat capability is now available");
      } else {
        showToast("info", `New capability available: ${cap}`);
      }
      return;
    }
    if (eventType === "capability_degraded") {
      // { capability, plugin, reason }
      const cap: string = data?.capability ?? "";
      const reason: string = data?.reason ?? "unknown";
      if (cap === "chat") {
        chatCapabilityAvailable = false;
        showToast("error", `Chat capability degraded: ${reason}. Check plugin status and try restarting.`);
      } else {
        showToast("warn", `Capability degraded: ${cap} (${reason})`);
      }
      return;
    }
    // Full registry update: { available, added, removed }
    if (eventType === "capability_registry_updated") {
      const available: string[] = payload.available ?? [];
      const wasAvailable = chatCapabilityAvailable;
      chatCapabilityAvailable = available.includes("chat");
      if (chatCapabilityAvailable && !wasAvailable) {
        showToast("success", "Chat capability restored");
      } else if (!chatCapabilityAvailable && wasAvailable) {
        showToast("warn", "Chat capability no longer available");
      }
      return;
    }

    if (!data?.session_id) return;

    // SOUL update event — capture name regardless of session
    if (eventType === "soul_updated" || eventType === "SOUL_UPDATED") {
      if (data.soul_name) {
        currentSoulName = data.soul_name;
        showToast("info", `SOUL switched to "${data.soul_name}"`);
      }
      return;
    }

    // ── Background idle-run session events ──
    // Intercept before active-session guard so we can toast without
    // switching the chat view. Covers both manual (dropdown) and
    // automatic (boredom-driven) idle runs in one unified path.

    // Automatic boredom runs: MessageReceived event carries background=true.
    if (eventType === "MessageReceived" && data?.background === true) {
      backgroundIdleSessions.add(data.session_id);
      const rawTag: string = data.tag ?? "";
      const tagLabel = rawTag ? rawTag.charAt(0).toUpperCase() + rawTag.slice(1) : "Idle";
      backgroundSessionTags.set(data.session_id, tagLabel);
      showToast("info", `DailyLife ${tagLabel} started`, 4000);
      return;
    }

    // Completion / error events for any tracked background session.
    if (backgroundIdleSessions.has(data.session_id)) {
      const tagLabel = backgroundSessionTags.get(data.session_id) ?? "Idle";
      if (eventType === "agent:awaiting_detach") {
        // Detached process is running — session is still alive, do NOT mark as completed
        awaitingDetachSessions = new Set([...awaitingDetachSessions, data.session_id]);
        showToast("info", `DailyLife ${tagLabel} running in background... (PID ${data.pid ?? "?"})`, 6000);
        return;
      }
      if (
        eventType === "agent:reply_ready" ||
        eventType === "agent:reply_interrupted"
      ) {
        // If we were awaiting detach, this is the final completion
        if (awaitingDetachSessions.has(data.session_id)) {
          awaitingDetachSessions = new Set([...awaitingDetachSessions].filter(s => s !== data.session_id));
        }
        backgroundIdleSessions.delete(data.session_id);
        backgroundSessionTags.delete(data.session_id);
        showToast("success", `DailyLife ${tagLabel} completed`, 5000);
        // Release the manual-trigger button if no other background runs active
        if (idleRunningTag && backgroundIdleSessions.size === 0) {
          idleRunningTag = null;
        }
        return;
      }
      if (
        eventType === "agent:reply_stream_error" ||
        eventType === "llm_error"
      ) {
        backgroundIdleSessions.delete(data.session_id);
        backgroundSessionTags.delete(data.session_id);
        showToast("error", `DailyLife ${tagLabel} failed`, 5000);
        if (idleRunningTag && backgroundIdleSessions.size === 0) {
          idleRunningTag = null;
        }
        return;
      }
      // Drop all other events for background sessions (tool calls, chunks, etc.)
      return;
    }

    if (data.session_id !== activeSessionId) return;

    // Capture channel_type from event payload
    const channelType: string | undefined = data.channel_type;

    switch (eventType) {
      case "message_queued":
        handleMessageQueued(data);
        break;
      case "message_dropped":
        handleMessageDropped(data);
        break;
      case "llm_reply_ready":
        handleLlmReplyReady(data);
        break;
      case "llm_tool_call":
        handleLlmToolCall(data);
        break;
      case "llm_tool_result":
        handleLlmToolResult(data);
        break;
      case "output_blocked":
        handleOutputBlocked(data);
        break;
      case "llm_error":
        handleLlmError(data);
        break;
      case "history_trimmed":
      case "HISTORY_TRIMMED":
        handleHistoryTrimmed(data);
        break;
      case "tool_auth_required":
        handleToolAuthRequired(data);
        break;
      // ── AgentHarness events (Phase B migration) ──
      case "agent:reply_stream_start":
        handleAgentStreamStart(data);
        break;
      case "agent:reply_chunk":
        handleAgentChunk(data);
        break;
      case "agent:reply_stream_done":
        handleAgentStreamDone(data);
        break;
      case "agent:reply_stream_error":
        handleAgentStreamError(data);
        break;
      case "agent:reply_ready":
        handleAgentReplyReady(data);
        break;
      case "agent:reply_interrupted":
        handleAgentReplyReady(data);
        break;
      case "tool:dispatched":
        handleAgentToolCall(data);
        break;
      case "tool:completed":
        handleAgentToolResult(data);
        break;
      case "tool:failed":
        handleAgentToolResult(data);
        break;
      case "agent:history_compressed":
        handleAgentHistoryCompressed(data);
        break;
      case "agent:awaiting_detach":
        handleAgentAwaitingDetach(data);
        break;
    }
  }

  function formatTokens(n: number): string {
    if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
    return String(n);
  }

  function handleHistoryTrimmed(data: any) {
    const sid: string = data.session_id;
    if (sid !== activeSessionId) return;

    // Support both new (AgentHarness) and legacy event shapes
    const removed = data.removed ?? data.trimmed_count ?? 0;
    const remaining = data.remaining ?? data.remaining_count ?? 0;
    const savedTokens = data.tokens_saved ?? 0;
    const usagePct = data.usage_pct ?? data.token_usage_pct ?? 0;

    let content: string;
    if (removed > 0) {
      const saved = formatTokens(savedTokens);
      content = `Compressed history — removed ${removed} messages, saved ~${saved} tokens. ${remaining} messages remaining`;
      if (usagePct > 0) {
        content += ` (${usagePct.toFixed(0)}% of context window).`;
      } else {
        content += ".";
      }
    } else {
      content = "Compression check passed — context within limits.";
    }

    messages = [...messages, {
      id: crypto.randomUUID(),
      type: "system_event" as MessageType,
      content,
      timestamp: new Date().toISOString(),
      sessionId: sid,
      status: "completed" as MessageStatus,
    }];
  }

  function handleAgentAwaitingDetach(data: any) {
    const sid: string = data.session_id;
    awaitingDetachSessions = new Set([...awaitingDetachSessions, sid]);
    // Keep session in "processing" state — do NOT go idle
    updateSessionStatus(sid, "processing");
    isLoading = true;
    // Add a system_event message so the user knows a background process is running
    messages = [...messages, {
      id: crypto.randomUUID(),
      type: "system_event",
      content: `**Running in background...** Process PID ${data.pid ?? "?"} is executing.`,
      timestamp: new Date().toISOString(),
      sessionId: sid,
      status: "completed",
    }];
  }

  async function handleToolAuthRequired(data: any) {
    // data = { session_id, auth_id, tool_name, arguments_summary, call_id }
    const { auth_id, tool_name, arguments_summary } = data;
    if (!auth_id || !tool_name) return;

    const result = await invoke("show_tool_auth_dialog", {
      authId: auth_id,
      toolName: tool_name,
      argumentsSummary: arguments_summary ?? "",
    }).catch((e: string) => {
      console.error("Tool auth dialog failed:", e);
      return "error";
    });

  }

  function handleLlmError(data: any) {
    // data = { session_id, original_message_id, error, soul_name }
    const sid: string = data.session_id;
    if (awaitingDetachSessions.has(sid)) {
      awaitingDetachSessions = new Set([...awaitingDetachSessions].filter(s => s !== sid));
    }
    isLoading = false;
    updateMessage(data.original_message_id, { status: "error" });
    updateSessionStatus(sid, "idle");
    messages = [...messages, {
      id: crypto.randomUUID(),
      type: "system_event",
      content: `**LLM Error:** ${data.error ?? "Unknown error"}`,
      timestamp: new Date().toISOString(),
      sessionId: data.session_id,
      status: "error" as MessageStatus,
    }];
  }

  function handleOutputBlocked(data: any) {
    const sid: string = data.session_id;
    const reason: string = data.reason ?? "unknown";
    const matchedRules: string[] = data.matched_rules ?? [];
    const isFailClosed: boolean = data.fail_closed === true;
    isLoading = false;
    updateSessionStatus(sid, "idle");
    messages = [...messages, {
      id: crypto.randomUUID(),
      type: "system_event",
      content: isFailClosed
        ? "OUTPUT_BLOCKED: Safety check failed — reply blocked. Please try again or contact admin."
        : `OUTPUT_BLOCKED: ${reason}`,
      timestamp: new Date().toISOString(),
      sessionId: sid,
      status: "error" as MessageStatus,
    }];
  }

  let activeStreamingMessageId: string | null = null;

  // --- 500ms cache window for /stop ---
  let stopWindowTimer: ReturnType<typeof setTimeout> | null = null;

  // --- Command dispatcher (§11.5) ---
  type CommandHandler = (args: string[]) => Promise<void>;

  interface CommandDef {
    name: string;
    aliases: string[];
    category: "non_llm" | "llm_dependent" | "interrupt";
    usage: string;
    description: string;
    handler: CommandHandler;
  }

  function parseCommand(input: string): { cmd: string; args: string[] } | null {
    const trimmed = input.trim();
    if (!trimmed.startsWith("/")) return null;
    const parts = trimmed.slice(1).split(/\s+/);
    return { cmd: parts[0]?.toLowerCase() ?? "", args: parts.slice(1) };
  }

  async function handleHelp(_args: string[]) {
    const lines = [
      "**Available commands:**",
      "",
      "**Non-LLM (immediate):**",
      "  `/help` — Show this help",
      "  `/session list` — List all sessions",
      "  `/session rename <name>` — Rename current session",
      "  `/session switch <id>` — Switch to a session",
      "  `/trace <id>` — Query trace chain (also see Maintenance page)",
      "  `/export` — Export conversation as text",
      "",
      "**LLM-dependent (queued):**",
      "  `/retry` — Retry last reply",
      "  `/retry --full` — Full retry (replay all tool calls)",
      "  `/edit <msg_index> <new_text>` — Edit and resend",
      "  `/session new` — Create new session",
      "  `/soul switch <name>` — Switch active SOUL",
      "",
      "**Interrupt:**",
      "  `/stop` — Stop generation (500ms grace window)",
      "  `/session close` — Close current session safely",
    ];
    messages = [...messages, {
      id: crypto.randomUUID(),
      type: "system_event",
      content: lines.join("\n"),
      timestamp: new Date().toISOString(),
      sessionId: activeSessionId,
      status: "completed" as MessageStatus,
    }];
  }

  async function handleSessionList(_args: string[]) {
    const list = sessions.map(s =>
      `  ${s.id === activeSessionId ? ">" : " "} ${s.id.slice(0, 8)} — ${s.title} (${s.status}, ${s.messageCount} msgs)`
    ).join("\n");
    messages = [...messages, {
      id: crypto.randomUUID(),
      type: "system_event",
      content: `**Sessions (${sessions.length} total):**\n${list}`,
      timestamp: new Date().toISOString(),
      sessionId: activeSessionId,
      status: "completed" as MessageStatus,
    }];
  }

  async function handleSessionRename(args: string[]) {
    const name = args.join(" ");
    if (!name) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: "Usage: `/session rename <new_name>`",
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
      }];
      return;
    }
    updateSession(activeSessionId, { title: name });
    try {
      await invoke("chat_session_rename", {
        agentKey: activeAgentKey || null,
        sessionId: activeSessionId,
        title: name,
      });
    } catch (e) {
      console.error("Failed to persist session title:", e);
    }
    messages = [...messages, {
      id: crypto.randomUUID(), type: "system_event",
      content: `Session renamed to "${name}".`,
      timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "completed" as MessageStatus,
    }];
  }

  async function handleSessionSwitch(args: string[]) {
    const id = args[0];
    if (!id) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: "Usage: `/session switch <session_id>`",
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
      }];
      return;
    }
    const target = sessions.find(s => s.id.startsWith(id));
    if (!target) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `Session "${id}" not found. Use \`/session list\` to see all sessions.`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
      }];
      return;
    }
    selectSession(target.id);
  }

  async function handleSessionNew(_args: string[]) {
    try {
      const id = await invoke<string>("chat_session_create", { agentKey: activeAgentKey || null });
      const count = sessions.length + 1;
      sessions = [{ id, title: `Chat ${count}`, messageCount: 0, status: "idle", createdAt: Date.now() }, ...sessions];
      activeSessionId = id;
    } catch (err: any) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `Failed to create session: ${err}`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
      }];
    }
  }

  async function handleSessionClose(_args: string[]) {
    try {
      await invoke("chat_session_close", { sessionId: activeSessionId });
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `Session closed.`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "completed" as MessageStatus,
      }];
      updateSession(activeSessionId, { status: "idle" });
    } catch (err: any) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `Failed to close session: ${err}`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
      }];
    }
  }

  async function handleRetry(args: string[]) {
    const isFull = args.includes("--full");
    // Find the last assistant message's trace_id to set as trace_prev
    const lastAssistantMsg = [...messages].reverse().find(m =>
      m.sessionId === activeSessionId && m.type.startsWith("assistant") && m.traceId
    );
    try {
      const msgId = await invoke<string>("chat_retry_last", {
        sessionId: activeSessionId,
        expectedVersion: null,
      });
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `Retry${isFull ? " (full replay)" : ""} triggered (message: ${msgId.slice(0, 8)}).`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "completed" as MessageStatus,
      }];
    } catch (err: any) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `Retry failed: ${err}`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
      }];
    }
  }

  async function handleEdit(args: string[]) {
    if (args.length < 2) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: "Usage: `/edit <msg_index> <new_text>` — e.g. `/edit 3 What is the weather?`",
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
      }];
      return;
    }
    const msgIndex = parseInt(args[0], 10);
    const newText = args.slice(1).join(" ");
    const sessionMsgs = messages.filter(m => m.sessionId === activeSessionId);
    if (isNaN(msgIndex) || msgIndex < 1 || msgIndex > sessionMsgs.length) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `Invalid message index. Use 1-${sessionMsgs.length}.`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
      }];
      return;
    }
    const targetMsg = sessionMsgs[msgIndex - 1];
    try {
      const eventId = await invoke<string>("chat_edit_message", {
        sessionId: activeSessionId,
        messageId: targetMsg.id,
        text: newText,
      });
      // Remove messages after the edited one, then send the replacement
      const targetIdx = messages.indexOf(targetMsg);
      messages = messages.slice(0, targetIdx + 1);
      // Mark the edited message
      updateMessage(targetMsg.id, { content: `${targetMsg.content}\n*(edited → will resend)*` });
      // Send the new text as a fresh message with trace_prev pointing to the original
      await invoke<string>("chat_send_message", {
        text: newText,
        sessionId: activeSessionId,
        tracePrev: targetMsg.traceId ?? null,
      });
    } catch (err: any) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `Edit failed: ${err}`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
      }];
    }
  }

  async function handleTraceQuery(args: string[]) {
    const traceId = args[0];
    if (!traceId) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: "Usage: `/trace <trace_id>` — query trace chain. Use message trace IDs shown in the UI.",
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "completed" as MessageStatus,
      }];
      return;
    }
    try {
      const chain = await invoke<Array<{ event_id: string; event_type: string; trace_id: string; timestamp_ms: number; session_id: string }>>("chat_trace_chain", { traceId });
      if (chain.length === 0) {
        messages = [...messages, {
          id: crypto.randomUUID(), type: "system_event",
          content: `No events found for trace ID: ${traceId.slice(0, 12)}...`,
          timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "completed" as MessageStatus,
        }];
        return;
      }
      const lines = [
        `**Trace Chain: ${traceId.slice(0, 12)}...**`,
        `Events in chain: ${chain.length}`,
        "",
        ...chain.map((e, i) => {
          const ts = new Date(e.timestamp_ms).toISOString().slice(11, 19);
          const shortTrace = e.trace_id.slice(0, 12);
          return `  ${i+1}. [${ts}] ${e.event_type} (trace: ${shortTrace})`;
        }),
      ];
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: lines.join("\n"),
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "completed" as MessageStatus,
      }];
    } catch (err: any) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `Trace query failed: ${err}`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
      }];
    }
  }

  async function handleExport(_args: string[]) {
    const sessionMsgs = messages.filter(m => m.sessionId === activeSessionId);
    const lines = sessionMsgs.map(m => {
      const role = m.type.startsWith("user") ? "User" : m.type.startsWith("assistant") ? "Assistant" : "System";
      return `[${m.timestamp.slice(0, 19)}] ${role}: ${m.content}`;
    });
    const text = lines.join("\n");
    // Copy to clipboard as a simple export
    try {
      await navigator.clipboard.writeText(text);
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `Exported ${sessionMsgs.length} messages to clipboard.`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "completed" as MessageStatus,
      }];
    } catch {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `Export to clipboard failed. Copy manually:\n\n${text}`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "completed" as MessageStatus,
      }];
    }
  }

  async function handleSoulSwitch(args: string[]) {
    const soulName = args.join(" ");
    if (!soulName) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: "Usage: `/soul switch <soul_name>`",
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
      }];
      return;
    }
    try {
      await invoke("update_soul", { nameOrPath: soulName });
      currentSoulName = soulName;
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `SOUL switched to "${soulName}".`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "completed" as MessageStatus,
      }];
    } catch (err: any) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `Failed to switch SOUL: ${err}`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
      }];
    }
  }

  // Command registry
  const commands: CommandDef[] = [
    { name: "help", aliases: ["h", "?"], category: "non_llm", usage: "/help", description: "Show available commands", handler: handleHelp },
    { name: "session", aliases: [], category: "non_llm", usage: "/session list|rename|switch|new|close", description: "Session management", handler: async (args) => {
      const sub = args[0]?.toLowerCase();
      if (sub === "list") await handleSessionList([]);
      else if (sub === "rename") await handleSessionRename(args.slice(1));
      else if (sub === "switch") await handleSessionSwitch(args.slice(1));
      else if (sub === "new") await handleSessionNew([]);
      else if (sub === "close") await handleSessionClose([]);
      else {
        messages = [...messages, {
          id: crypto.randomUUID(), type: "system_event",
          content: "Usage: `/session list|rename|switch|new|close`. Try `/help`.",
          timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
        }];
      }
    }},
    { name: "retry", aliases: ["r"], category: "llm_dependent", usage: "/retry [--full]", description: "Retry last reply", handler: handleRetry },
    { name: "edit", aliases: ["e"], category: "llm_dependent", usage: "/edit <msg_index> <text>", description: "Edit and resend", handler: handleEdit },
    { name: "export", aliases: [], category: "non_llm", usage: "/export", description: "Export conversation", handler: handleExport },
    { name: "trace", aliases: ["tc"], category: "non_llm", usage: "/trace <trace_id>", description: "Query trace chain", handler: handleTraceQuery },
    { name: "soul", aliases: [], category: "llm_dependent", usage: "/soul switch|info <name>", description: "Switch SOUL or show info", handler: async (args) => {
      if (args[0] === "switch") await handleSoulSwitch(args.slice(1));
      else if (args[0] === "info") {
        try {
          const soulRaw = await invoke<any>("get_soul_raw", {});
          const soulInfo = await invoke<any>("get_soul_info", {});
          soulDescription = soulRaw?.description ?? soulRaw?.identity ?? "(no description)";
          soulDetailExpanded = true;
          messages = [...messages, {
            id: crypto.randomUUID(), type: "system_event",
            content: `**SOUL: ${currentSoulName || "unknown"}**\n  Description: ${soulDescription}\n  Last changed: ${soulInfo?.last_changed ?? "unknown"}`,
            timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "completed" as MessageStatus,
          }];
        } catch (err: any) {
          messages = [...messages, {
            id: crypto.randomUUID(), type: "system_event",
            content: `Failed to load SOUL info: ${err}`,
            timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
          }];
        }
      }
      else {
        messages = [...messages, {
          id: crypto.randomUUID(), type: "system_event",
          content: currentSoulName
            ? `Current SOUL: **${currentSoulName}**`
            : "No SOUL currently loaded. Use `/soul switch <name>` to set one.",
          timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "completed" as MessageStatus,
        }];
      }
    }},
  ];

  function getCommand(name: string): CommandDef | undefined {
    return commands.find(c => c.name === name || c.aliases.includes(name));
  }

  async function dispatchCommand(parsed: { cmd: string; args: string[] }): Promise<boolean> {
    const cmdDef = getCommand(parsed.cmd);
    if (!cmdDef) return false; // unknown command
    await cmdDef.handler(parsed.args);
    return true;
  }

  async function stopGeneration() {
    // 500ms cache window (§11.5): if LLM_STREAM_DONE arrives within 500ms, it's "complete"
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

    try {
      await invoke("chat_stop_generation", { sessionId: activeSessionId });
    } catch { /* non-fatal */ }

    isLoading = false;
    updateSessionStatus(activeSessionId, "idle");
    inputText = "";
  }

  async function sendMessage(text?: string) {
    text = (text ?? inputText).trim();
    if (!text) return;

    if (text === "/stop") {
      await stopGeneration();
      return;
    }

    closeSkillPicker();
    inputText = "";
    if (chatInputRef) chatInputRef.value = "";
    const tempId = crypto.randomUUID();

    const userMsg: Message = {
      id: tempId,
      type: "user_text",
      content: text,
      timestamp: new Date().toISOString(),
      sessionId: activeSessionId,
      status: "pending",
    };
    messages = [...messages, userMsg];
    isLoading = true;
    updateSessionStatus(activeSessionId, "processing");

    try {
      const eventId = await invoke<string>("chat_send_message", { text, sessionId: activeSessionId });
      messages = messages.map(m => (m.id === tempId ? { ...m, id: eventId, status: "sent" } : m));
    } catch (err: any) {
      // Handle rate limiting (429)
      const errStr = typeof err === "string" ? err : (err?.message ?? String(err));
      if (errStr.startsWith("429:")) {
        // Parse retry_after from the message
        const match = errStr.match(/in (\d+) seconds/);
        if (match) {
          rateLimitCountdown = parseInt(match[1], 10);
        } else {
          rateLimitCountdown = 10; // fallback
        }
        updateMessage(tempId, { status: "error", content: `Rate limited. Try again in ${rateLimitCountdown}s.` });
      } else {
        updateMessage(tempId, { status: "error", content: `Error: ${errStr}` });
      }
      isLoading = false;
      updateSessionStatus(activeSessionId, "idle");
    }
  }

  function handleChatInputKeydown(e: KeyboardEvent) {
    // Skill picker keyboard navigation (keydown on <chat-input>)
    if (!showSkillPicker) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      skillPickerIndex = Math.min(skillPickerIndex + 1, skillPickerResults.length - 1);
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      skillPickerIndex = Math.max(skillPickerIndex - 1, 0);
      return;
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (skillPickerResults[skillPickerIndex]) {
        applySkillPickerSelection(skillPickerResults[skillPickerIndex].name);
      }
      return;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      if (skillPickerResults[skillPickerIndex]) {
        applySkillPickerSelection(skillPickerResults[skillPickerIndex].name);
      }
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      closeSkillPicker();
      return;
    }
  }

  function handleGlobalKeydown(e: KeyboardEvent) {
    // Ctrl+Enter or Meta+Enter: send message (if not already handled by <chat-input>)
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter" && !e.isComposing && !e.defaultPrevented) {
      e.preventDefault();
      sendMessage();
    }
  }

  // Wire <chat-input> web component events
  $effect(() => {
    const el = chatInputRef;
    if (!el) return;

    function onSend(e: Event) {
      const text = (e as CustomEvent).detail?.text || "";
      sendMessage(text);
    }
    function onStop() {
      stopGeneration();
    }
    function onInput(e: Event) {
      const text = (e as CustomEvent).detail?.text || "";
      inputText = text;
      updateSkillPicker();
    }

    el.addEventListener("send", onSend);
    el.addEventListener("stop", onStop);
    el.addEventListener("input", onInput);
    el.addEventListener("keydown", handleChatInputKeydown);

    return () => {
      el.removeEventListener("send", onSend);
      el.removeEventListener("stop", onStop);
      el.removeEventListener("input", onInput);
      el.removeEventListener("keydown", handleChatInputKeydown);
    };
  });

  // Apply prefill text from external navigation (e.g., Finance skill card).
  // Uses a sequence counter so we can detect a fresh prefill even after
  // the component has mounted and activeSessionId changes asynchronously.
  let lastPrefillSeq = $state(0);

  $effect(() => {
    if (prefillInput && prefillSeq !== lastPrefillSeq && activeSessionId) {
      inputText = prefillInput;
      lastPrefillSeq = prefillSeq;
      if (chatInputRef) {
        chatInputRef.value = prefillInput;
        chatInputRef.focus();
      }
    }
  });

  onMount(async () => {
    const unsub1 = await listen("event:processed", handleEventProcessed);
    unlisteners.push(unsub1);

    // Check chat capability directly in case we missed runtime events.
    // Be optimistic on failure — runtime capability events are authoritative.
    try {
      const caps = await invoke<{ capability: string }[]>("get_capabilities");
      chatCapabilityAvailable = caps.some((c) => c.capability === "chat");
    } catch {
      // Keep the default (true). If chat is truly unavailable, the gateway
      // will send capability_removed/degraded events once the listener is up.
    }

    // Load agents first (sets activeAgentKey, triggers session load for the right agent).
    await loadAgents();

    // Only create a fresh session when arriving with a prefill (e.g. from a
    // skill card), so the command doesn't leak into an existing conversation.
    // Otherwise open the last session — don't auto-create empty sessions.
    if (prefillInput && prefillInput.length > 0) {
      await createSession();
    } else if (sessions.length > 0) {
      selectSession(sessions[0].id);
    }

    await loadSoulInfo();
    await loadSkills();
    await loadIdleAvailability();
    window.addEventListener("keydown", handleGlobalKeydown);
  });

  onDestroy(() => {
    for (const fn of unlisteners) fn();
    window.removeEventListener("keydown", handleGlobalKeydown);
  });
</script>

<div class="chat-layout">
  <!-- Agents Panel — IM-style contact list -->
  <aside class="agents-panel">
    <div class="agents-header">
      <span>Agents</span>
    </div>
    <div class="agents-list">
      {#if agentList.length === 0}
        <div class="agents-empty">No agents</div>
      {:else}
        <div class="agent-section-label">Chat Agents</div>
        {#each agentList.filter(a => a.provider) as agent (agent.key)}
          {@const initials = agent.display_name.charAt(0).toUpperCase()}
          {@const hue = (agent.key.split('').reduce((h, c) => h + c.charCodeAt(0), 0) * 137) % 360}
          <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
          <div
            class="agent-contact"
            class:active={agent.key === activeAgentKey}
            onclick={() => handleAgentSelect(agent.key)}
            onkeydown={(e) => e.key === 'Enter' && handleAgentSelect(agent.key)}
            role="button"
            tabindex="0"
            title={agent.display_name}
          >
            <div class="agent-avatar" style="background: hsl({hue}, 55%, 48%);">
              {initials}
            </div>
            <div class="agent-contact-info">
              <span class="agent-contact-name">{agent.display_name}</span>
            </div>
          </div>
        {/each}
      {/if}
    </div>
  </aside>

  <!-- Left Sidebar with Sessions -->
  <aside class="session-panel">
    <div class="panel-header">
      <div class="panel-header-actions">
        <div class="panel-header-left">
          <div class="daily-life-dropdown">
            <button
              class="idle-run-btn daily-life-trigger"
              onclick={() => dailyLifeOpen = !dailyLifeOpen}
              title="Daily Life"
              disabled={idleRunningTag !== null}
            >
              {idleRunningTag ? "⏳" : "Daily Life ▾"}
            </button>
            {#if dailyLifeOpen}
              <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
              <div class="dropdown-backdrop" onclick={() => dailyLifeOpen = false} onkeydown={() => {}} role="presentation"></div>
              <div class="dropdown-menu">
                <button class="dropdown-item" onclick={() => { dailyLifeOpen = false; startIdleRun("work"); }} disabled={!idleAvailability[activeAgentKey]?.work}>
                  💼 Work
                </button>
                <button class="dropdown-item" onclick={() => { dailyLifeOpen = false; startIdleRun("study"); }} disabled={!idleAvailability[activeAgentKey]?.study}>
                  📚 Study
                </button>
                <button class="dropdown-item" onclick={() => { dailyLifeOpen = false; startIdleRun("fun"); }} disabled={!idleAvailability[activeAgentKey]?.fun}>
                  🎲 Fun
                </button>
              </div>
            {/if}
          </div>
          <button class="explore-btn" onclick={startExplore} title="Explore" disabled={isExploring}>
            {isExploring ? "⏳" : "🔍"}
          </button>
        </div>
        <button class="new-btn" onclick={createSession} title="New chat">+</button>
      </div>
    </div>
      <div class="session-list">
        {#each paginatedSessions as session}
          <div class="session-row" class:active={session.id === activeSessionId}>
            <button
              class="session-item"
              class:active={session.id === activeSessionId}
              onclick={() => selectSession(session.id)}
            >
              <span class="session-title">{session.title}</span>
              <span class="session-meta">
                {#if session.createdAt && session.lastActiveAt}
                  {#if new Date(session.createdAt).toLocaleDateString() === new Date(session.lastActiveAt).toLocaleDateString()}
                    {new Date(session.createdAt).toLocaleDateString()}
                  {:else}
                    {new Date(session.createdAt).toLocaleDateString()} - {new Date(session.lastActiveAt).toLocaleDateString()}
                  {/if}
                {/if}
              </span>
            </button>
            <button
              class="session-delete-btn"
              title="Delete session"
              onclick={(e) => { e.stopPropagation(); deletingSessionId = session.id; }}
              disabled={deletingSessionId === session.id}
            >&times;</button>
          </div>
        {/each}
        {#if sessions.length === 0 && sessionsLoaded}
          <div class="session-empty">No sessions yet</div>
        {:else if !sessionsLoaded}
          <div class="session-empty">Loading...</div>
        {/if}
      </div>
      {#if totalPages > 1}
        <div class="pagination">
          <button
            class="page-btn"
            disabled={currentPage <= 1}
            onclick={() => currentPage = Math.max(1, currentPage - 1)}
          >&laquo;</button>
          <span class="page-info">{currentPage} / {totalPages}</span>
          <button
            class="page-btn"
            disabled={currentPage >= totalPages}
            onclick={() => currentPage = Math.min(totalPages, currentPage + 1)}
          >&raquo;</button>
        </div>
      {/if}
      {#if deletingSessionId}
        <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
        <div class="confirm-overlay" onclick={() => deletingSessionId = null} onkeydown={() => {}} role="button" tabindex="0">
          <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
          <div class="confirm-dialog" onclick={(e) => e.stopPropagation()} onkeydown={() => {}} role="dialog" tabindex="-1">
            <p>Delete this session?</p>
            <div class="confirm-actions">
              <button class="confirm-cancel" onclick={() => deletingSessionId = null}>Cancel</button>
              <button class="confirm-delete" onclick={() => deleteSession(deletingSessionId!)}>Delete</button>
            </div>
          </div>
        </div>
      {/if}
  </aside>

  <!-- Main Chat Area -->
  <div class="chat-main">
    <!-- Chat Header -->
    <header class="chat-header">
      <h2>
        {#if editingTitle && activeSession}
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <input
            class="title-edit-input"
            type="text"
            bind:value={editTitleValue}
            onkeydown={async (e) => {
              if (e.key === 'Enter') { e.preventDefault(); await saveTitle(); }
              else if (e.key === 'Escape') { editingTitle = false; }
            }}
            onblur={saveTitle}
            autofocus
          />
        {:else}
          <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
          <span
            class="title-text"
            title="Click to rename"
            onclick={() => { if (activeSession) { editingTitle = true; editTitleValue = activeSession.title ?? ''; } }}
            onkeydown={(e) => { if (e.key === 'Enter' && activeSession) { editingTitle = true; editTitleValue = activeSession.title ?? ''; } }}
            role="button"
            tabindex="0"
          >
            {activeSession?.title ?? "Select a session"}
          </span>
        {/if}
        {#if currentSoulName}
          <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
          <span class="soul-badge" title="Current SOUL. Click for details." onclick={() => soulDetailExpanded = !soulDetailExpanded} onkeydown={(e) => e.key === 'Enter' && (soulDetailExpanded = !soulDetailExpanded)} role="button" tabindex="0">
            {currentSoulName}
          </span>
        {/if}
        {#if soulDetailExpanded && currentSoulName}
          <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
          <div class="soul-detail-popup" onclick={(e) => e.stopPropagation()} onkeydown={() => {}}>
            <strong>{currentSoulName}</strong>
            <p class="soul-desc">{soulDescription || "No description available."}</p>
            <p class="soul-hint">Use <code>/soul info</code> for full details. <code>/soul switch &lt;name&gt;</code> to change.</p>
          </div>
        {/if}
      </h2>
      <div class="chat-header-end">
        <span class="chat-status" class:loading={isProcessing}>
          {isProcessing ? "Processing..." : "Ready"}
        </span>
      </div>
    </header>

    <!-- Messages -->
    <div class="message-area" bind:this={messageAreaEl} onscroll={handleScroll}>
      {#if activeMessages.length === 0}
        <div class="empty-state">
          <p>No messages yet. Start a conversation above.</p>
          {#if !chatCapabilityAvailable}
            <p class="hint warning">Chat capability is disabled. New sessions cannot be created until re-enabled.</p>
          {:else}
            <p class="hint">Tip: Chat capability is not active until the runtime detects a chat plugin.</p>
          {/if}
        </div>
      {:else}
        {#each activeMessages as msg (msg.id)}
          {@const isUser = msg.type.startsWith("user")}
          {@const isAssistant = msg.type.startsWith("assistant") && !(msg.type === "assistant_tool_call" || msg.type === "assistant_tool_result")}
          {@const isSystem = msg.type.startsWith("system") || msg.type.startsWith("security")}
          {@const isToolCall = msg.type === "assistant_tool_call"}
          {@const isArchived = msg.archived === true || archivedMsgIds.has(msg.id)}
          <div
            class="message"
            class:user={isUser}
            class:assistant={isAssistant}
            class:system={isSystem}
            class:tool-call={isToolCall}
            class:interrupted={msg.status === "interrupted"}
            class:archived={isArchived}
          >
            {#if isAssistant}
              <span class="msg-label">{agentList.find(a => a.key === activeAgentKey)?.display_name ?? "Assistant"}</span>
            {/if}
            {#if isToolCall && msg.toolCall}
              <ToolCallCard tool={msg.toolCall} />
            {:else}
              <div
                class="msg-bubble"
                class:streaming={msg.type === "assistant_streaming"}
                class:status-error={msg.status === "error"}
              >
                {#if isAssistant}
                  <div class="markdown-body">
                    {@html marked.parse(escapeMarkdown(msg.content))}
                    {#if msg.type === "assistant_streaming"}
                      <span class="cursor"></span>
                    {/if}
                  </div>
                {:else}
                  <p>{msg.content}</p>
                {/if}
              </div>
            {/if}
            <span class="msg-time">
              {msg.timestamp.slice(11, 19)}
              {#if msg.channelType}
                <span class="channel-tag">{msg.channelType}</span>
              {/if}
              {#if isArchived}
                <span class="archived-label">archived</span>
              {/if}
              {#if msg.status === "pending"}
                <span class="msg-status pending">sending...</span>
              {:else if msg.status === "error"}
                <span class="msg-status error">failed</span>
              {/if}
              {#if msg.traceId}
                <span class="trace-tag" title="trace_id: {msg.traceId}">#{msg.traceId.slice(0, 8)}</span>
              {/if}
            </span>
          </div>
        {/each}
      {/if}
    </div>

    <!-- Input Area -->
    <div class="input-area">
      <chat-input
        bind:this={chatInputRef}
        placeholder={!chatCapabilityAvailable
          ? "Chat capability unavailable..."
          : !activeAgentHasProvider
            ? "Configure a provider for this agent first..."
            : rateLimitCountdown > 0
              ? `Rate limited — wait ${rateLimitCountdown}s...`
              : "Type a message... (Ctrl+Enter to send)"}
        rows="1"
        buttontext="Send"
        stoptext="Stop"
        disabled={isProcessing || rateLimitCountdown > 0 || !chatCapabilityAvailable || !activeAgentHasProvider}
        processing={isProcessing ? "" : undefined}
        ratelimit={rateLimitCountdown > 0 ? rateLimitCountdown : undefined}
      ></chat-input>
      {#if showSkillPicker}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="skill-picker" onkeydown={(e: KeyboardEvent) => e.stopPropagation()}>
          {#each skillPickerResults as skill, i}
            <!-- svelte-ignore a11y_no_static_element_interactions -->
            <div
              class="skill-picker-item"
              class:selected={i === skillPickerIndex}
              onclick={() => applySkillPickerSelection(skill.name)}
              onmouseenter={() => skillPickerIndex = i}
            >
              <span class="skill-picker-name">/{skill.name}</span>
              <span class="skill-picker-desc">{skill.description}</span>
            </div>
          {/each}
          {#if skillPickerResults.length === 0}
            <div class="skill-picker-empty">No matching skills</div>
          {/if}
        </div>
      {/if}
    </div>
  </div>

</div>

<!-- Toast notifications -->
<div class="toast-container">
  {#each toasts as toast (toast.id)}
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
    <div class="toast toast-{toast.type}" onclick={() => dismissToast(toast.id)} onkeydown={(e) => e.key === 'Enter' && dismissToast(toast.id)} role="button" tabindex="0">
      <span class="toast-icon">
        {#if toast.type === "success"}&#10003;{:else if toast.type === "warn"}&#9888;{:else if toast.type === "error"}&#10007;{:else}&#8505;{/if}
      </span>
      <span class="toast-msg">{toast.message}</span>
    </div>
  {/each}
</div>

<style>
  .chat-layout {
    display: flex;
    height: 100%;
    gap: 0;
  }

  /* ── Agents Panel (IM contact list style) ── */
  .agents-panel {
    width: 200px;
    min-width: 200px;
    background: var(--bg-card);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
  }

  .agents-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 12px;
    border-bottom: 1px solid var(--border);
    font-size: 14px;
    font-weight: 600;
  }

  .agents-list {
    flex: 1;
    overflow-y: auto;
    padding: 4px;
  }

  .agents-empty {
    padding: 16px 12px;
    text-align: center;
    color: var(--fg-dim);
    font-size: 12px;
  }

  .agents-empty-sub {
    padding: 8px 12px;
    text-align: center;
    color: var(--fg-dim);
    font-size: 11px;
    font-style: italic;
  }

  .agent-section-label {
    padding: 8px 10px 4px;
    font-size: 10px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--fg-dim);
  }

  .agent-contact {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border-radius: 8px;
    cursor: pointer;
    transition: background 0.12s;
    margin-bottom: 2px;
  }

  .agent-contact:hover {
    background: var(--bg-hover);
  }

  .agent-contact.active {
    background: var(--bg-hover);
  }

  .agent-avatar {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #fff;
    font-size: 15px;
    font-weight: 600;
    flex-shrink: 0;
    user-select: none;
  }

  .agent-contact-info {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .agent-contact-name {
    font-size: 13px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .session-panel {
    width: 240px;
    min-width: 240px;
    background: var(--bg-card);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
  }

  .panel-header {
    display: flex;
    align-items: center;
    padding: 12px;
    border-bottom: 1px solid var(--border);
  }

  .panel-header-actions {
    display: flex;
    justify-content: space-between;
    align-items: center;
    width: 100%;
  }

  .panel-header-left {
    display: flex;
    gap: 4px;
    align-items: center;
  }

  .daily-life-dropdown {
    position: relative;
  }

  .idle-run-btn {
    height: 28px;
    padding: 0 8px;
    display: flex;
    align-items: center;
    justify-content: center;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    cursor: pointer;
    font-size: 12px;
    font-weight: 500;
    line-height: 1;
    white-space: nowrap;
    color: var(--fg);
  }

  .idle-run-btn:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .idle-run-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .dropdown-backdrop {
    position: fixed;
    inset: 0;
    z-index: 9;
  }

  .dropdown-menu {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 10;
    min-width: 130px;
    background: var(--bg-card, var(--bg));
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 4px;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.2);
  }

  .dropdown-item {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 6px 10px;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: var(--fg);
    font-size: 12px;
    font-weight: 500;
    cursor: pointer;
    text-align: left;
  }

  .dropdown-item:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .dropdown-item:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .new-btn {
    width: 28px;
    height: 28px;
    display: flex;
    align-items: center;
    justify-content: center;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    cursor: pointer;
    font-size: 18px;
    line-height: 1;
    color: var(--fg);
  }

  .new-btn:hover {
    background: var(--bg-hover);
  }

  .explore-btn {
    width: 28px;
    height: 28px;
    display: flex;
    align-items: center;
    justify-content: center;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
  }

  .explore-btn:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .explore-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .sidebar-tabs {
    display: flex;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }

  .tab-btn {
    flex: 1;
    padding: 8px 4px;
    border: none;
    background: transparent;
    cursor: pointer;
    font-size: 12px;
    font-weight: 500;
    color: var(--text-secondary);
    border-bottom: 2px solid transparent;
    transition: color 0.15s, border-color 0.15s;
  }

  .tab-btn:hover {
    color: var(--text-primary);
    background: var(--bg-hover);
  }

  .tab-btn.active {
    color: var(--accent);
    border-bottom-color: var(--accent);
  }

  .sidebar-tab-content {
    flex: 1;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }

  .sidebar-tab-content :global(.depth-panel) {
    width: auto;
    min-width: 0;
    border-left: none;
    flex: 1;
  }

  .session-list {
    flex: 1;
    overflow-y: auto;
    padding: 4px;
  }

  .session-item {
    display: flex;
    flex-direction: column;
    width: 100%;
    padding: 8px 12px;
    border: none;
    background: transparent;
    text-align: left;
    cursor: pointer;
    margin-bottom: 0;
  }

  .session-item:hover {
    background: transparent;
  }

  .session-row {
    display: flex;
    align-items: stretch;
    border-radius: 6px;
    margin-bottom: 2px;
  }

  .session-row.active {
    background: var(--bg-hover);
    border-left: 2px solid var(--accent);
  }

  .session-row:hover {
    background: var(--bg-hover);
  }

  .session-row .session-item {
    flex: 1;
    border-radius: 0;
  }

  .session-row.active .session-item {
    background: transparent;
    border-left: none;
  }

  .session-delete-btn {
    width: 28px;
    border: none;
    background: transparent;
    color: var(--fg-dim);
    cursor: pointer;
    font-size: 16px;
    line-height: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    opacity: 0;
    transition: opacity 0.15s;
    border-radius: 0 6px 6px 0;
  }

  .session-row:hover .session-delete-btn {
    opacity: 0.6;
  }

  .session-delete-btn:hover {
    opacity: 1 !important;
    background: rgba(239, 68, 68, 0.12);
    color: var(--red, #ef4444);
  }

  .session-delete-btn:disabled {
    opacity: 0.3 !important;
  }

  .session-empty {
    padding: 16px 12px;
    text-align: center;
    color: var(--fg-dim);
    font-size: 12px;
  }

  .pagination {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 8px;
    border-top: 1px solid var(--border);
    flex-shrink: 0;
  }

  .page-btn {
    padding: 2px 8px;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--fg);
    font-size: 13px;
    cursor: pointer;
    line-height: 1.4;
  }

  .page-btn:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .page-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .page-info {
    font-size: 11px;
    color: var(--fg-dim);
    min-width: 48px;
    text-align: center;
  }

  .confirm-overlay {
    position: fixed;
    inset: 0;
    z-index: 2000;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .confirm-dialog {
    background: var(--bg-card, #1e1e2e);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 20px 24px;
    min-width: 240px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.3);
  }

  .confirm-dialog p {
    margin: 0 0 16px 0;
    font-size: 14px;
    font-weight: 500;
    text-align: center;
  }

  .confirm-actions {
    display: flex;
    gap: 8px;
    justify-content: center;
  }

  .confirm-cancel {
    padding: 6px 16px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
    font-size: 13px;
    cursor: pointer;
  }

  .confirm-cancel:hover {
    background: var(--bg-hover);
  }

  .confirm-delete {
    padding: 6px 16px;
    border: none;
    border-radius: 6px;
    background: var(--red, #ef4444);
    color: #fff;
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
  }

  .confirm-delete:hover {
    background: #dc2626;
  }

  .session-title {
    font-size: 13px;
    font-weight: 500;
  }

  .session-meta {
    font-size: 11px;
    color: var(--fg-dim);
    margin-top: 2px;
  }

  .chat-main {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .chat-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 4px 16px;
    border-bottom: 1px solid var(--border);
    background: var(--bg);
  }

  .chat-header h2 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .title-text {
    cursor: pointer;
    border-radius: 4px;
    padding: 1px 4px;
    transition: background 0.15s;
  }

  .title-text:hover {
    background: var(--bg-hover);
  }

  .title-edit-input {
    font-size: 15px;
    font-weight: 600;
    font-family: inherit;
    border: 1px solid var(--accent, #3b82f6);
    border-radius: 4px;
    padding: 2px 6px;
    background: var(--bg);
    color: var(--fg);
    outline: none;
    max-width: 400px;
  }

  .soul-badge {
    display: inline-block;
    font-size: 11px;
    font-weight: 500;
    padding: 2px 10px;
    margin-left: 8px;
    border-radius: 10px;
    background: var(--accent-muted);
    color: var(--accent);
    vertical-align: middle;
  }

  .chat-status {
    font-size: 12px;
    color: var(--fg-dim);
  }

  .chat-status.loading {
    color: var(--accent, #3b82f6);
  }

  .message-area {
    flex: 1;
    overflow-y: auto;
    padding-left: 16px;
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--fg-dim);
    text-align: center;
  }

  .empty-state .hint {
    font-size: 12px;
    margin-top: 8px;
    opacity: 0.7;
  }

  .empty-state .hint.warning {
    color: var(--yellow);
    opacity: 1;
    font-weight: 500;
  }

  .message {
    margin-bottom: 6px;
    display: flex;
    flex-direction: column;
  }

  .message.user {
    align-items: flex-end;
  }

  .message.assistant {
    align-items: flex-start;
  }

  .message.system {
    align-items: center;
  }

  .message.tool-call {
    align-items: flex-start;
    max-width: 75%;
  }

  .msg-bubble {
    max-width: 70%;
    padding: 10px 16px;
    border-radius: 18px;
    line-height: 1.5;
  }

  .message.user .msg-bubble {
    background: var(--accent, #3b82f6);
    color: #fff;
    border-bottom-right-radius: 4px;
  }

  .message.assistant .msg-bubble {
    background: var(--bg-card);
    color: var(--fg);
    border: 1px solid var(--border);
    border-bottom-left-radius: 4px;
  }

  .message.system .msg-bubble {
    background: transparent;
    font-size: 12px;
    color: var(--fg-dim);
  }

  .msg-bubble p {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .msg-time {
    font-size: 11px;
    color: var(--fg-dim);
    margin-top: 2px;
    padding: 0 4px;
    display: flex;
    gap: 4px;
    align-items: center;
  }

  .msg-label {
    font-size: 11px;
    font-weight: 600;
    color: var(--fg-dim);
    margin-bottom: 2px;
    padding: 0 4px;
  }

  .msg-status {
    font-size: 10px;
  }

  .msg-status.pending {
    color: var(--accent, #3b82f6);
  }

  .msg-status.error {
    color: var(--red, #ef4444);
  }

  .interrupted .msg-bubble {
    border: 1px dashed var(--yellow);
  }

  .msg-bubble.status-error {
    border-color: var(--red);
    background: var(--red-muted);
  }

  .msg-bubble.streaming {
    border: 1px solid var(--accent);
    box-shadow: 0 0 0 1px var(--accent-muted);
  }

  @keyframes blink {
    50% { opacity: 0; }
  }

  .cursor {
    display: inline-block;
    width: 2px;
    height: 1em;
    background: currentColor;
    margin-left: 1px;
    animation: blink 0.8s step-end infinite;
    vertical-align: text-bottom;
  }

  .input-area {
    position: relative;
    display: flex;
    gap: 8px;
    padding: 8px 16px;
    border-top: 1px solid var(--border);
    background: var(--bg);
    --chat-input-bg: var(--bg-card);
    --chat-input-fg: var(--fg);
    --chat-input-border: var(--border);
    --chat-input-accent: var(--accent);
    --chat-input-accent-hover: var(--accent-hover);
    --chat-input-red: var(--red);
    --chat-input-yellow: var(--yellow);
    --chat-input-disabled-bg: var(--bg);
  }

  /* Skill picker dropdown */
  .skill-picker {
    position: absolute;
    bottom: 100%;
    left: 16px;
    right: 16px;
    margin-bottom: 4px;
    max-height: 260px;
    overflow-y: auto;
    background: var(--bg-card, #1e1e2e);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 -4px 20px rgba(0, 0, 0, 0.3);
    z-index: 100;
  }

  .skill-picker-item {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 12px;
    cursor: pointer;
    border-bottom: 1px solid var(--border);
    transition: background 0.1s;
  }

  .skill-picker-item:last-child {
    border-bottom: none;
  }

  .skill-picker-item:hover,
  .skill-picker-item.selected {
    background: var(--bg-hover, rgba(59, 130, 246, 0.15));
  }

  .skill-picker-name {
    font-family: "SF Mono", "Fira Code", monospace;
    font-size: 13px;
    font-weight: 600;
    color: var(--accent, #3b82f6);
    white-space: nowrap;
    min-width: fit-content;
  }

  .skill-picker-desc {
    font-size: 12px;
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .skill-picker-empty {
    padding: 12px;
    text-align: center;
    font-size: 12px;
    color: var(--text-muted);
  }

  /* -------- T7.6 Additions -------- */

  .chat-header-end {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .soul-detail-popup {
    position: absolute;
    top: 48px;
    left: 16px;
    z-index: 100;
    width: 300px;
    padding: 12px 16px;
    background: var(--bg-card);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-lg);
    font-size: 12px;
    line-height: 1.5;
  }

  .soul-detail-popup strong {
    display: block;
    margin-bottom: 4px;
    font-size: 13px;
  }

  .soul-desc {
    margin: 0 0 6px 0;
    color: var(--fg-dim);
  }

  .soul-hint {
    margin: 0;
    font-size: 11px;
    color: var(--fg-dim);
  }

  .soul-hint code {
    font-size: 10px;
    background: var(--bg-card);
    padding: 1px 4px;
    border-radius: 3px;
  }

  .message.archived {
    opacity: 0.5;
  }

  .message.archived:hover {
    opacity: 0.8;
  }

  .channel-tag {
    font-size: 9px;
    padding: 1px 5px;
    border-radius: var(--radius-sm);
    background: var(--bg-hover);
    color: var(--fg-dim);
    text-transform: uppercase;
    font-weight: 500;
  }

  .archived-label {
    font-size: 9px;
    padding: 1px 5px;
    border-radius: var(--radius-sm);
    background: var(--yellow-muted);
    color: var(--yellow);
    font-weight: 500;
  }

  .trace-tag {
    font-size: 9px;
    padding: 1px 5px;
    border-radius: var(--radius-sm);
    background: var(--bg-hover);
    color: var(--fg-dim);
    font-family: "SF Mono", "Fira Code", monospace;
    cursor: help;
  }

  .toast-container {
    position: fixed;
    top: 12px;
    right: 12px;
    z-index: 2000;
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-width: 360px;
  }

  .toast {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 14px;
    border-radius: 8px;
    font-size: 12px;
    cursor: pointer;
    box-shadow: 0 4px 12px rgba(0,0,0,0.15);
    animation: toast-in 0.25s ease-out;
    line-height: 1.4;
  }

  @keyframes toast-in {
    from { opacity: 0; transform: translateX(20px); }
    to { opacity: 1; transform: translateX(0); }
  }

  .toast-info {
    background: rgba(108, 140, 255, 0.12);
    border: 1px solid rgba(108, 140, 255, 0.25);
    color: var(--accent);
  }

  .toast-success {
    background: var(--green-muted);
    border: 1px solid rgba(74, 222, 128, 0.25);
    color: var(--green);
  }

  .toast-warn {
    background: var(--yellow-muted);
    border: 1px solid rgba(250, 204, 21, 0.25);
    color: var(--yellow);
  }

  .toast-error {
    background: var(--red-muted);
    border: 1px solid rgba(248, 113, 113, 0.25);
    color: var(--red);
  }

  .toast-icon {
    flex-shrink: 0;
    font-size: 14px;
    width: 18px;
    text-align: center;
  }

  .toast-msg {
    flex: 1;
    word-break: break-word;
  }

  /* ── Markdown body (assistant messages) ──
     Child selectors use :global() because the HTML is injected via
     {@html marked.parse(...)} — Svelte can't scope styles to it. */
  .markdown-body {
    line-height: 1.6;
    word-break: break-word;
  }
  :global(.markdown-body p) { margin: 0 0 0.5em 0; }
  :global(.markdown-body p:last-child) { margin-bottom: 0; }
  :global(.markdown-body ul),
  :global(.markdown-body ol) { margin: 0.25em 0; padding-left: 1.5em; }
  :global(.markdown-body li) { margin: 0.15em 0; }
  :global(.markdown-body code) {
    background: rgba(128,128,128,0.12);
    border-radius: 3px;
    padding: 0.15em 0.35em;
    font-size: 0.88em;
    font-family: ui-monospace, SFMono-Regular, SF Mono, Menlo, Consolas, monospace;
  }
  :global(.markdown-body pre) {
    background: rgba(128,128,128,0.08);
    border: 1px solid rgba(128,128,128,0.18);
    border-radius: 6px;
    padding: 0.75em 1em;
    overflow-x: auto;
    margin: 0.5em 0;
  }
  :global(.markdown-body pre code) { background: none; padding: 0; border-radius: 0; font-size: 0.85em; }
  :global(.markdown-body blockquote) {
    border-left: 3px solid rgba(128,128,128,0.3);
    margin: 0.5em 0;
    padding: 0.25em 0.75em;
    color: #666;
  }
  :global(.markdown-body table) { border-collapse: collapse; margin: 0.5em 0; font-size: 0.92em; }
  :global(.markdown-body th),
  :global(.markdown-body td) {
    border: 1px solid rgba(128,128,128,0.25);
    padding: 0.4em 0.6em;
    text-align: left;
  }
  :global(.markdown-body th) { background: rgba(128,128,128,0.08); font-weight: 600; }
  :global(.markdown-body a) { color: var(--accent-hover); text-decoration: none; }
  :global(.markdown-body a:hover) { text-decoration: underline; }
  :global(.markdown-body hr) { border: none; border-top: 1px solid rgba(128,128,128,0.2); margin: 0.75em 0; }
  :global(.markdown-body h1),
  :global(.markdown-body h2),
  :global(.markdown-body h3),
  :global(.markdown-body h4),
  :global(.markdown-body h5),
  :global(.markdown-body h6) { margin: 0.6em 0 0.3em 0; line-height: 1.3; }
  :global(.markdown-body h1) { font-size: 1.35em; }
  :global(.markdown-body h2) { font-size: 1.2em; }
  :global(.markdown-body h3) { font-size: 1.1em; }
  :global(.markdown-body h4),
  :global(.markdown-body h5),
  :global(.markdown-body h6) { font-size: 1em; }
  :global(.markdown-body img) { max-width: 100%; border-radius: 4px; }
</style>
