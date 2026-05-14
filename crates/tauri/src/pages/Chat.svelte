<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";

  interface Session {
    id: string;
    title: string;
    messageCount: number;
    status: string;
  }

  interface Message {
    id: string;
    type: "user_text" | "user_command" | "assistant_text" | "assistant_streaming" | "assistant_tool_call" | "assistant_tool_result" | "system_event" | "security_alert";
    content: string;
    timestamp: string;
    sessionId: string;
  }

  // Hardcoded sessions for skeleton development
  let sessions: Session[] = [
    { id: "s1", title: "API Design Discussion", messageCount: 12, status: "idle" },
    { id: "s2", title: "Bug Investigation", messageCount: 5, status: "active" },
  ];

  let activeSessionId = $state("s1");
  let messages = $state<Message[]>([]);
  let inputText = $state("");
  let isLoading = $state(false);
  let unlisteners: (() => void)[] = [];

  const activeSession = $derived(sessions.find(s => s.id === activeSessionId));

  function selectSession(id: string) {
    activeSessionId = id;
    // TODO: load session messages from backend
  }

  async function sendMessage() {
    const text = inputText.trim();
    if (!text) return;
    inputText = "";
    // TODO: invoke backend to send message
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  }

  onMount(async () => {
    // TODO: subscribe to events when backend is connected
    // const unsub = await listen("message:received", (e) => { ... });
    // unlisteners.push(unsub);
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
      <button class="new-btn" disabled title="Coming soon">+</button>
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
      <span class="chat-status" class:loading={isLoading}>
        {isLoading ? "Processing..." : "Ready"}
      </span>
    </header>

    <!-- Messages -->
    <div class="message-area">
      {#if messages.length === 0}
        <div class="empty-state">
          <p>No messages yet. Start a conversation above.</p>
          <p class="hint">Tip: Chat capability is not active until the runtime detects a chat plugin.</p>
        </div>
      {:else}
        {#each messages as msg}
          <div class="message" class:user={msg.type.startsWith("user")} class:assistant={msg.type.startsWith("assistant")} class:system={msg.type.startsWith("system") || msg.type.startsWith("security")}>
            <div class="msg-bubble">
              <p>{msg.content}</p>
            </div>
            <span class="msg-time">{msg.timestamp}</span>
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
        disabled={isLoading}
      ></textarea>
      <button class="send-btn" onclick={sendMessage} disabled={!inputText.trim() || isLoading}>
        Send
      </button>
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

  .new-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
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
</style>
