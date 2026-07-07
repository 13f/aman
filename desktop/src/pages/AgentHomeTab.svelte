<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";

  const { agentKey }: { agentKey: string } = $props();

  interface SessionInfo {
    id: string;
    title: string;
    state: string;
    messageCount: number;
    lastActiveAt: number;
    isProcessing: boolean;
  }

  interface WorkflowInstance {
    id: string;
    name: string;
    status: string;
    startedAt: string;
  }

  let sessions = $state<SessionInfo[]>([]);
  let workflows = $state<WorkflowInstance[]>([]);
  let loading = $state(true);
  let processingSessions = $state<Set<string>>(new Set());
  let abortingId = $state<string | null>(null);

  let unlisteners: (() => void) = [];
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  async function loadSessions() {
    try {
      const list = await invoke<Array<{
        id: string; state: string; message_count: number;
        created_at: number; last_active_at: number | null;
        title?: string;
      }>>("chat_session_list_db", { agentKey });

      sessions = list.map((s, i) => ({
        id: s.id,
        title: s.title || (s.id.length > 8 ? `Session ${s.id.slice(0, 8)}` : `Session ${i + 1}`),
        state: s.state,
        messageCount: s.message_count,
        lastActiveAt: s.last_active_at ?? s.created_at,
        isProcessing: processingSessions.has(s.id) || s.state === "processing",
      }));
    } catch {
      // Fallback to gateway API
      try {
        const list = await invoke<Array<{
          id: string; state: string; message_count: number;
          created_at: number; last_active_at: number | null;
          title?: string;
        }>>("chat_session_list", { agentKey });
        sessions = list.map((s, i) => ({
          id: s.id,
          title: s.title || (s.id.length > 8 ? `Session ${s.id.slice(0, 8)}` : `Session ${i + 1}`),
          state: s.state,
          messageCount: s.message_count,
          lastActiveAt: s.last_active_at ?? s.created_at,
          isProcessing: processingSessions.has(s.id) || s.state === "processing",
        }));
      } catch {
        sessions = [];
      }
    }
  }

  async function loadWorkflows() {
    try {
      const v = await invoke<any>("get_workflow_instances");
      const items = (v?.instances ?? v ?? []) as any[];
      workflows = items
        .filter(w => w.agent_id === agentKey || w.agent_key === agentKey)
        .filter(w => w.status === "running" || w.status === "pending")
        .map(w => ({
          id: w.id,
          name: w.name ?? w.workflow ?? "workflow",
          status: w.status,
          startedAt: w.started_at ?? new Date().toISOString(),
        }));
    } catch {
      workflows = [];
    }
  }

  async function pollProcessingSessions() {
    // Check active runtime agent status to know which sessions are busy.
    try {
      const agent = await invoke<{
        active_session_id?: string;
        status?: string;
        system_state?: string;
      } | null>("get_runtime_agent", { agentId: agentKey });
      if (agent?.active_session_id && (agent.status === "Busy" || agent.system_state !== "idle")) {
        processingSessions = new Set([agent.active_session_id]);
      } else {
        processingSessions = new Set();
      }
    } catch { /* ignore */ }
    await loadSessions();
  }

  async function abortSession(sessionId: string) {
    abortingId = sessionId;
    try {
      // Kill the session process outright.  chat_stop_generation only
      // aborts the *current LLM turn* and shows up as an extra "/stop"
      // message for workflow-backed sessions; killing is the reliable
      // "stop everything" action.
      await invoke("chat_kill_session", { sessionId });
    } catch { /* ignore */ }
    abortingId = null;
    await loadSessions();
  }

  async function killSession(sessionId: string) {
    abortingId = sessionId;
    try {
      await invoke("chat_kill_session", { sessionId });
    } catch { /* ignore */ }
    abortingId = null;
    await loadSessions();
  }

  async function cancelWorkflow(id: string) {
    try {
      await invoke("cancel_workflow", { id });
    } catch { /* ignore */ }
    await loadWorkflows();
  }

  function timestampLabel(ts: number): string {
    if (!ts) return "";
    const d = new Date(ts * 1000);
    const now = Date.now();
    const diff = now - d.getTime();
    if (diff < 60_000) return "just now";
    if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
    if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
    return d.toLocaleDateString();
  }

  function handleEventProcessed(event: { payload: any }) {
    const data = event.payload;
    if (!data) return;
    if (data.agent_id && data.agent_id !== agentKey) return;

    // Refresh on any session-relevant event.
    const et = data.event_type ?? "";
    if (
      et.startsWith("llm_") ||
      et.includes("tool:") ||
      et.includes("agent:reply") ||
      et.includes("session") ||
      et.includes("idle")
    ) {
      void pollProcessingSessions();
      void loadWorkflows();
    }
  }

  onMount(async () => {
    loading = true;
    await Promise.all([pollProcessingSessions(), loadWorkflows()]);
    loading = false;

    // Poll every 5 s to catch processing state changes the SSE might miss.
    pollTimer = setInterval(() => {
      void pollProcessingSessions();
      void loadWorkflows();
    }, 5000);

    const unlisten = await listen("event:processed", handleEventProcessed);
    unlisteners.push(unlisten);
  });

  onDestroy(() => {
    for (const u of unlisteners) u();
    unlisteners = [];
    if (pollTimer) clearInterval(pollTimer);
  });

  const activeSessions = $derived(sessions.filter(s => s.isProcessing));
  const idleSessions = $derived(sessions.filter(s => !s.isProcessing));
  const hasContent = $derived(activeSessions.length > 0 || workflows.length > 0);
</script>

<div class="home-tab">
  {#if loading}
    <div class="empty-state">
      <div class="spinner"></div>
      <p>Loading tasks…</p>
    </div>
  {:else if !hasContent}
    <div class="empty-state">
      <p class="empty-icon">✓</p>
      <p class="empty-title">No active tasks</p>
      <p class="empty-desc">{agentKey} is idle. Start a chat or trigger a workflow to see activity here.</p>
    </div>
  {:else}
    {#if activeSessions.length > 0}
      <section class="task-section">
        <h3 class="section-title">Active ({activeSessions.length})</h3>
        <ul class="task-list">
          {#each activeSessions as session (session.id)}
            <li class="task-item processing">
              <div class="task-main">
                <span class="task-status-dot"></span>
                <div class="task-info">
                  <span class="task-name">{session.title}</span>
                  <span class="task-meta">
                    Processing · {session.messageCount} msgs · {timestampLabel(session.lastActiveAt)}
                  </span>
                </div>
              </div>
              <div class="task-actions">
                <button
                  class="task-btn kill"
                  disabled={abortingId === session.id}
                  onclick={() => killSession(session.id)}
                  title="Kill session"
                >
                  {abortingId === session.id ? "…" : "Kill"}
                </button>
              </div>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if workflows.length > 0}
      <section class="task-section">
        <h3 class="section-title">Workflows ({workflows.length})</h3>
        <ul class="task-list">
          {#each workflows as wf (wf.id)}
            <li class="task-item workflow">
              <div class="task-main">
                <span class="task-status-dot wf-dot"></span>
                <div class="task-info">
                  <span class="task-name">{wf.name}</span>
                  <span class="task-meta">{wf.status}</span>
                </div>
              </div>
              <div class="task-actions">
                <button
                  class="task-btn kill"
                  onclick={() => cancelWorkflow(wf.id)}
                  title="Cancel workflow"
                >
                  Cancel
                </button>
              </div>
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if idleSessions.length > 0}
      <section class="task-section">
        <h3 class="section-title">Idle sessions ({idleSessions.length})</h3>
        <ul class="task-list">
          {#each idleSessions as session (session.id)}
            <li class="task-item idle">
              <div class="task-main">
                <span class="task-name muted">{session.title}</span>
                <span class="task-meta">{session.messageCount} msgs · {timestampLabel(session.lastActiveAt)}</span>
              </div>
            </li>
          {/each}
        </ul>
      </section>
    {/if}
  {/if}
</div>

<style>
  .home-tab {
    padding: 20px 24px;
    height: 100%;
    overflow-y: auto;
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    gap: 8px;
    color: var(--fg-dim, #9ca3af);
  }

  .empty-icon {
    font-size: 36px;
    color: var(--green, #22c55e);
    margin: 0;
  }

  .empty-title {
    font-size: 16px;
    font-weight: 600;
    color: var(--fg, #e5e7eb);
    margin: 0;
  }

  .empty-desc {
    font-size: 13px;
    text-align: center;
    max-width: 300px;
    line-height: 1.5;
  }

  .spinner {
    width: 28px;
    height: 28px;
    border: 3px solid var(--border, rgba(255, 255, 255, 0.1));
    border-top-color: var(--accent, #6366f1);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .task-section {
    margin-bottom: 24px;
  }

  .section-title {
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--fg-dim, #9ca3af);
    margin: 0 0 10px;
  }

  .task-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .task-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    background: var(--bg-card, rgba(255, 255, 255, 0.03));
    border: 1px solid var(--border, rgba(255, 255, 255, 0.06));
    border-radius: 10px;
    gap: 12px;
  }

  .task-item.processing {
    border-left: 3px solid var(--green, #22c55e);
  }

  .task-item.workflow {
    border-left: 3px solid #b39dfc;
  }

  .task-main {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
    flex: 1;
  }

  .task-status-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--green, #22c55e);
    box-shadow: 0 0 8px color-mix(in srgb, var(--green, #22c55e) 60%, transparent);
    animation: pulse 1.5s ease-in-out infinite;
    flex-shrink: 0;
  }

  .wf-dot {
    background: #b39dfc;
    box-shadow: 0 0 8px rgba(179, 157, 252, 0.5);
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.5; transform: scale(1.3); }
  }

  .task-info {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .task-name {
    font-size: 14px;
    font-weight: 500;
    color: var(--fg, #e5e7eb);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .task-name.muted {
    color: var(--fg-dim, #9ca3af);
    font-weight: 400;
  }

  .task-meta {
    font-size: 11px;
    color: var(--fg-dim, #9ca3af);
  }

  .task-actions {
    display: flex;
    gap: 6px;
    flex-shrink: 0;
  }

  .task-btn {
    padding: 5px 14px;
    font-size: 12px;
    font-weight: 500;
    border-radius: 6px;
    border: 1px solid transparent;
    cursor: pointer;
    transition: opacity 0.15s;
  }

  .task-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .task-btn.stop {
    background: transparent;
    border-color: var(--yellow, #f0a020);
    color: var(--yellow, #f0a020);
  }

  .task-btn.stop:hover:not(:disabled) {
    background: color-mix(in srgb, var(--yellow, #f0a020) 12%, transparent);
  }

  .task-btn.kill {
    background: transparent;
    border-color: #ef4444;
    color: #ef4444;
  }

  .task-btn.kill:hover:not(:disabled) {
    background: rgba(239, 68, 68, 0.12);
  }
</style>
