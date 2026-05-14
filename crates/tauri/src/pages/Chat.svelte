<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";
  import ToolCallCard from "./ToolCallCard.svelte";
  import type { ToolCallData } from "./ToolCallCard.svelte";

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
  }

  interface Session {
    id: string;
    title: string;
    messageCount: number;
    status: "idle" | "processing";
  }

  let sessions = $state<Session[]>([
    { id: "default", title: "Default Chat", messageCount: 0, status: "idle" },
  ]);
  let activeSessionId = $state("default");
  let messages = $state<Message[]>([]);
  let inputText = $state("");
  let isLoading = $state(false);
  let currentSoulName = $state("");
  let rateLimitCountdown = $state(0);
  let unlisteners: (() => void)[] = [];
  let messageAreaEl: HTMLDivElement | undefined = $state();
  let autoScroll = $state(true);
  let chatCapabilityAvailable = $state(true);

  const activeSession = $derived(sessions.find(s => s.id === activeSessionId));
  const isProcessing = $derived(
    isLoading || messages.some(m => m.sessionId === activeSessionId && (m.status === "pending" || m.status === "streaming"))
  );

  function updateMessage(id: string, patch: Partial<Message>) {
    messages = messages.map(m => (m.id === id ? { ...m, ...patch } : m));
  }

  function updateSession(id: string, patch: Partial<Session>) {
    sessions = sessions.map(s => (s.id === id ? { ...s, ...patch } : s));
  }

  function updateSessionStatus(id: string, status: "idle" | "processing") {
    updateSession(id, { status });
  }

  function selectSession(id: string) {
    activeSessionId = id;
    const count = messages.filter(m => m.sessionId === id).length;
    updateSession(id, { messageCount: count });
  }

  function createSession() {
    const id = crypto.randomUUID();
    const count = sessions.length + 1;
    sessions = [...sessions, { id, title: `Chat ${count}`, messageCount: 0, status: "idle" }];
    activeSessionId = id;
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

  // Loading timeout — force reset if no events arrive
  $effect(() => {
    if (isLoading) {
      const timeout = setTimeout(() => { isLoading = false; }, 10000);
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
    const reply: Message = {
      id: crypto.randomUUID(),
      type: "assistant_text",
      content: data.reply,
      timestamp: new Date().toISOString(),
      sessionId: data.session_id,
      status: "completed",
    };
    messages = [...messages, reply];
    updateMessage(data.original_message_id, { status: "completed" });
    isLoading = false;
    updateSessionStatus(data.session_id, "idle");
    if (data.soul_name) {
      currentSoulName = data.soul_name;
    }
  }

  function handleLlmToolCall(data: any) {
    // data = { session_id, call_id, tool_name, arguments }
    const callId: string = data.call_id;
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

  function handleEventProcessed(e: any) {
    const payload = e.payload;
    const eventType: string = payload.event_type;
    const data = payload.payload;

    // Capability events use different payload structure
    if (eventType === "capability_removed") {
      // Individual event: { capability: "chat" }
      if (data?.capability === "chat") {
        chatCapabilityAvailable = false;
        // Phase 4.5: close active tabs, clear message buffer
        // but DON'T clear persistent state (history can be restored later)
        messages = messages.filter(m => m.sessionId !== activeSessionId);
        sessions = sessions.filter(s => s.status === "idle");
        // Reset to default session
        activeSessionId = "default";
      }
      return;
    }
    if (eventType === "capability_available") {
      if (data?.capability === "chat") {
        chatCapabilityAvailable = true;
      }
      return;
    }
    // Full registry update: { available, added, removed }
    if (eventType === "capability_registry_updated") {
      const available: string[] = payload.available ?? [];
      chatCapabilityAvailable = available.includes("chat");
      return;
    }

    if (!data?.session_id) return;
    if (data.session_id !== activeSessionId) return;

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
    }
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
      "  `/debug` — Show session debug info",
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
      const id = await invoke<string>("chat:session_create");
      const count = sessions.length + 1;
      sessions = [...sessions, { id, title: `Chat ${count}`, messageCount: 0, status: "idle" }];
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
      await invoke("chat:session_close", { sessionId: activeSessionId });
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
    try {
      const msgId = await invoke<string>("chat:retry_last", { sessionId: activeSessionId });
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
      const eventId = await invoke<string>("chat:edit_message", {
        sessionId: activeSessionId,
        messageId: targetMsg.id,
        text: newText,
      });
      // Remove messages after the edited one, then send the replacement
      const targetIdx = messages.indexOf(targetMsg);
      messages = messages.slice(0, targetIdx + 1);
      // Mark the edited message
      updateMessage(targetMsg.id, { content: `${targetMsg.content}\n*(edited → will resend)*` });
      // Send the new text as a fresh message
      await invoke<string>("chat:send_message", { text: newText, sessionId: activeSessionId });
    } catch (err: any) {
      messages = [...messages, {
        id: crypto.randomUUID(), type: "system_event",
        content: `Edit failed: ${err}`,
        timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "error" as MessageStatus,
      }];
    }
  }

  async function handleDebug(_args: string[]) {
    const sessionMsgs = messages.filter(m => m.sessionId === activeSessionId);
    const info = [
      "**Debug info:**",
      `  Active session: ${activeSessionId.slice(0, 8)}`,
      `  Sessions total: ${sessions.length}`,
      `  Messages in session: ${sessionMsgs.length}`,
      `  Capability: ${chatCapabilityAvailable ? "available" : "unavailable"}`,
      `  Loading: ${isLoading}`,
      `  Processing: ${isProcessing}`,
      `  Rate limit countdown: ${rateLimitCountdown}s`,
    ];
    messages = [...messages, {
      id: crypto.randomUUID(), type: "system_event",
      content: info.join("\n"),
      timestamp: new Date().toISOString(), sessionId: activeSessionId, status: "completed" as MessageStatus,
    }];
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
    { name: "debug", aliases: ["dbg"], category: "non_llm", usage: "/debug", description: "Show debug info", handler: handleDebug },
    { name: "export", aliases: [], category: "non_llm", usage: "/export", description: "Export conversation", handler: handleExport },
    { name: "soul", aliases: [], category: "llm_dependent", usage: "/soul switch <name>", description: "Switch active SOUL", handler: async (args) => {
      if (args[0] === "switch") await handleSoulSwitch(args.slice(1));
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
      await invoke("chat:stop_generation", { sessionId: activeSessionId });
    } catch { /* non-fatal */ }

    isLoading = false;
    updateSessionStatus(activeSessionId, "idle");
    inputText = "";
  }

  async function sendMessage() {
    const text = inputText.trim();
    if (!text) return;

    if (text === "/stop") {
      await stopGeneration();
      return;
    }

    inputText = "";
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
      const eventId = await invoke<string>("chat:send_message", { text, sessionId: activeSessionId });
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

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  }

  onMount(async () => {
    const unsub1 = await listen("event:processed", handleEventProcessed);
    unlisteners.push(unsub1);
  });

  onDestroy(() => {
    for (const fn of unlisteners) fn();
  });
</script>

<div class="chat-layout">
  <!-- Session Panel -->
  <aside class="session-panel">
    <div class="panel-header">
      <h2>Sessions</h2>
      <button class="new-btn" onclick={createSession} title="New chat">+</button>
    </div>
    <div class="session-list">
      {#each sessions as session}
        <button
          class="session-item"
          class:active={session.id === activeSessionId}
          onclick={() => selectSession(session.id)}
        >
          <span class="session-title">{session.title}</span>
          <span class="session-meta">{session.messageCount} msgs &middot; {session.status}</span>
        </button>
      {/each}
    </div>
  </aside>

  <!-- Main Chat Area -->
  <div class="chat-main">
    <!-- Chat Header -->
    <header class="chat-header">
      <h2>
        {activeSession?.title ?? "Select a session"}
        {#if currentSoulName}
          <span class="soul-badge">{currentSoulName}</span>
        {/if}
      </h2>
      <span class="chat-status" class:loading={isProcessing}>
        {isProcessing ? "Processing..." : "Ready"}
      </span>
    </header>

    <!-- Messages -->
    <div class="message-area" bind:this={messageAreaEl} onscroll={handleScroll}>
      {#if messages.length === 0}
        <div class="empty-state">
          <p>No messages yet. Start a conversation above.</p>
          {#if !chatCapabilityAvailable}
            <p class="hint warning">Chat capability is disabled. New sessions cannot be created until re-enabled.</p>
          {:else}
            <p class="hint">Tip: Chat capability is not active until the runtime detects a chat plugin.</p>
          {/if}
        </div>
      {:else}
        {#each messages as msg (msg.id)}
          {@const isUser = msg.type.startsWith("user")}
          {@const isAssistant = msg.type.startsWith("assistant") && !(msg.type === "assistant_tool_call" || msg.type === "assistant_tool_result")}
          {@const isSystem = msg.type.startsWith("system") || msg.type.startsWith("security")}
          {@const isToolCall = msg.type === "assistant_tool_call"}
          <div
            class="message"
            class:user={isUser}
            class:assistant={isAssistant}
            class:system={isSystem}
            class:tool-call={isToolCall}
            class:interrupted={msg.status === "interrupted"}
          >
            {#if isToolCall && msg.toolCall}
              <ToolCallCard tool={msg.toolCall} />
            {:else}
              <div
                class="msg-bubble"
                class:streaming={msg.type === "assistant_streaming"}
                class:status-error={msg.status === "error"}
              >
                <p>
                  {msg.content}
                  {#if msg.type === "assistant_streaming"}
                    <span class="cursor"></span>
                  {/if}
                </p>
              </div>
            {/if}
            <span class="msg-time">
              {msg.timestamp.slice(11, 19)}
              {#if msg.status === "pending"}
                <span class="msg-status pending">sending...</span>
              {:else if msg.status === "error"}
                <span class="msg-status error">failed</span>
              {/if}
            </span>
          </div>
        {/each}
      {/if}
    </div>

    <!-- Input Area -->
    <div class="input-area">
      <textarea
        bind:value={inputText}
        onkeydown={handleKeydown}
        placeholder={!chatCapabilityAvailable
          ? "Chat capability unavailable..."
          : rateLimitCountdown > 0
            ? `Rate limited — wait ${rateLimitCountdown}s...`
            : "Type a message... (Enter to send, Shift+Enter for newline)"}
        rows="1"
        disabled={isProcessing || rateLimitCountdown > 0 || !chatCapabilityAvailable}
      ></textarea>
      {#if rateLimitCountdown > 0}
        <button class="rate-limited-btn" disabled>{rateLimitCountdown}s</button>
      {:else if isProcessing}
        <button class="stop-btn" onclick={stopGeneration}>Stop</button>
      {:else}
        <button class="send-btn" onclick={sendMessage} disabled={!inputText.trim()}>Send</button>
      {/if}
    </div>
  </div>
</div>

<style>
  .chat-layout {
    display: flex;
    height: 100%;
    gap: 0;
  }

  .session-panel {
    width: 240px;
    min-width: 240px;
    background: var(--surface-secondary, #f5f5f5);
    border-right: 1px solid var(--border, #ddd);
    display: flex;
    flex-direction: column;
  }

  .panel-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 12px;
    border-bottom: 1px solid var(--border, #ddd);
  }

  .panel-header h2 {
    margin: 0;
    font-size: 14px;
    font-weight: 600;
  }

  .new-btn {
    width: 24px;
    height: 24px;
    border: 1px solid var(--border, #ddd);
    border-radius: 4px;
    background: var(--surface, #fff);
    cursor: pointer;
    font-size: 16px;
    line-height: 1;
  }

  .new-btn:hover {
    background: var(--accent-light, #dbeafe);
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
    border-radius: 6px;
    background: transparent;
    text-align: left;
    cursor: pointer;
    margin-bottom: 2px;
  }

  .session-item:hover {
    background: var(--surface-hover, #e8e8e8);
  }

  .session-item.active {
    background: var(--accent-light, #dbeafe);
  }

  .session-title {
    font-size: 13px;
    font-weight: 500;
    color: var(--text-primary, #111);
  }

  .session-meta {
    font-size: 11px;
    color: var(--text-secondary, #666);
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
    padding: 12px 16px;
    border-bottom: 1px solid var(--border, #ddd);
    background: var(--surface, #fff);
  }

  .chat-header h2 {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
  }

  .soul-badge {
    display: inline-block;
    font-size: 11px;
    font-weight: 500;
    padding: 1px 8px;
    margin-left: 8px;
    border-radius: 10px;
    background: var(--accent-light, #dbeafe);
    color: var(--accent, #3b82f6);
    vertical-align: middle;
  }

  .chat-status {
    font-size: 12px;
    color: var(--text-secondary, #666);
  }

  .chat-status.loading {
    color: var(--accent, #3b82f6);
  }

  .message-area {
    flex: 1;
    overflow-y: auto;
    padding: 16px;
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--text-secondary, #666);
    text-align: center;
  }

  .empty-state .hint {
    font-size: 12px;
    margin-top: 8px;
    opacity: 0.7;
  }

  .empty-state .hint.warning {
    color: var(--warning, #b45309);
    opacity: 1;
    font-weight: 500;
  }

  .message {
    margin-bottom: 12px;
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
    padding: 8px 14px;
    border-radius: 12px;
    background: var(--surface-secondary, #f0f0f0);
  }

  .message.user .msg-bubble {
    background: var(--accent, #3b82f6);
    color: #fff;
    border-bottom-right-radius: 4px;
  }

  .message.assistant .msg-bubble {
    background: var(--surface-secondary, #f0f0f0);
    border-bottom-left-radius: 4px;
  }

  .message.system .msg-bubble {
    background: transparent;
    font-size: 12px;
    color: var(--text-secondary, #888);
  }

  .msg-bubble p {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .msg-time {
    font-size: 11px;
    color: var(--text-secondary, #999);
    margin-top: 2px;
    padding: 0 4px;
    display: flex;
    gap: 4px;
    align-items: center;
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
    border: 1px dashed var(--warning, #f59e0b);
  }

  .msg-bubble.status-error {
    border-color: var(--red, #ef4444);
    background: var(--red-light, #fef2f2);
  }

  .msg-bubble.streaming {
    border: 1px solid var(--accent, #3b82f6);
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
    display: flex;
    gap: 8px;
    padding: 12px 16px;
    border-top: 1px solid var(--border, #ddd);
    background: var(--surface, #fff);
  }

  .input-area textarea {
    flex: 1;
    padding: 8px 12px;
    border: 1px solid var(--border, #ddd);
    border-radius: 8px;
    resize: none;
    font-family: inherit;
    font-size: 13px;
    line-height: 1.4;
    min-height: 36px;
    max-height: 120px;
  }

  .input-area textarea:disabled {
    background: var(--surface-secondary, #f5f5f5);
  }

  .send-btn {
    padding: 8px 20px;
    border: none;
    border-radius: 8px;
    background: var(--accent, #3b82f6);
    color: #fff;
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
    align-self: flex-end;
  }

  .send-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .send-btn:hover:not(:disabled) {
    background: var(--accent-hover, #2563eb);
  }

  .stop-btn {
    padding: 8px 20px;
    border: 1px solid var(--red, #ef4444);
    border-radius: 8px;
    background: transparent;
    color: var(--red, #ef4444);
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
    align-self: flex-end;
  }

  .stop-btn:hover {
    background: var(--red-light, #fef2f2);
  }

  .rate-limited-btn {
    padding: 8px 20px;
    border: 1px solid var(--warning, #f59e0b);
    border-radius: 8px;
    background: var(--warning-light, #fef3c7);
    color: var(--warning, #b45309);
    font-size: 13px;
    font-weight: 600;
    cursor: not-allowed;
    align-self: flex-end;
  }
</style>
