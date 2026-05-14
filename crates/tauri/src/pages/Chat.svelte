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
  let unlisteners: (() => void)[] = [];
  let messageAreaEl: HTMLDivElement | undefined = $state();
  let autoScroll = $state(true);

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

  // --- Event handlers ---

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
    }
  }

  let activeStreamingMessageId: string | null = null;

  async function stopGeneration() {
    const streamingId = activeStreamingMessageId;
    if (streamingId) {
      setTimeout(() => {
        const msg = messages.find(m => m.id === streamingId);
        if (msg && msg.status === "streaming") {
          updateMessage(streamingId, { status: "interrupted" });
          activeStreamingMessageId = null;
        }
      }, 500);
    }

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
      updateMessage(tempId, { status: "error", content: `Error: ${err}` });
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
      <h2>{activeSession?.title ?? "Select a session"}</h2>
      <span class="chat-status" class:loading={isProcessing}>
        {isProcessing ? "Processing..." : "Ready"}
      </span>
    </header>

    <!-- Messages -->
    <div class="message-area" bind:this={messageAreaEl} onscroll={handleScroll}>
      {#if messages.length === 0}
        <div class="empty-state">
          <p>No messages yet. Start a conversation above.</p>
          <p class="hint">Tip: Chat capability is not active until the runtime detects a chat plugin.</p>
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
        placeholder="Type a message... (Enter to send, Shift+Enter for newline)"
        rows="1"
        disabled={isProcessing}
      ></textarea>
      {#if isProcessing}
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
</style>
