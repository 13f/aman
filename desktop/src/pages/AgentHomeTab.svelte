<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { t } from "../lib/i18n.svelte";

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

  // ── Skill quick-run (Daily Life) ──────────────────────────────
  // Whether each skill tag is available for this agent (drives the
  // disabled state on the empty-state buttons).
  // Mirrors gateway `agents_idle_availability`: a tag is available iff an
  // idle_run skill with that tag exists. (Work skills are discovery skills
  // that query the kanban themselves, so skill existence is the only gate —
  // NOT a non-empty work queue.) Defaults stay false until the first load
  // lands so we never flash an enabled button for a missing skill.
  let idleAvailability = $state<Record<string, boolean>>({
    work: false, study: false, fun: false, prize: false,
  });
  // Becomes true after the first successful load. Until then every
  // button stays disabled so we don't show "enabled" for a tag whose
  // availability we don't yet know.
  let availabilityReady = $state(false);
  // Tag currently being launched/running — disables all run buttons.
  let idleRunningTag = $state<string | null>(null);
  // Background idle-run sessions we started, tracked for toast feedback.
  let backgroundIdleSessions = $state<Set<string>>(new Set());
  let backgroundSessionTags = $state<Map<string, string>>(new Map());
  // Lightweight transient toast (mirrors <chat-input> pattern).
  let runToast = $state<{ kind: "info" | "error" | "success"; msg: string } | null>(null);
  let runToastTimer: ReturnType<typeof setTimeout> | null = null;
  // session_id → live detached child PIDs (e.g. exec(detach:true) scripts).
  // Fed by the agent_states:updated SSE snapshot's running_children.
  let pidsBySession = $state<Record<string, number[]>>({});

  let unlisteners: Array<() => void> = [];
  let pollTimer: ReturnType<typeof setInterval> | null = null;

  const SKILL_TAGS: Array<{ tag: string; icon: string; labelKey: string }> = [
    { tag: "work",  icon: "💼", labelKey: "home.skill_work" },
    { tag: "study", icon: "📚", labelKey: "home.skill_study" },
    { tag: "fun",   icon: "🎲", labelKey: "home.skill_fun" },
    { tag: "prize", icon: "🏆", labelKey: "home.skill_prize" },
  ];

  function skillLabel(tag: string): string {
    const entry = SKILL_TAGS.find(s => s.tag === tag);
    const key = entry?.labelKey ?? "";
    const trad = t(key);
    return trad === key ? tag.charAt(0).toUpperCase() + tag.slice(1) : trad;
  }

  async function loadIdleAvailability() {
    try {
      const v: any = await invoke("list_idle_availability");
      const entry: Record<string, boolean> | undefined = v?.agents?.[agentKey];
      if (entry) {
        idleAvailability = {
          work: !!entry.work,
          study: !!entry.study,
          fun: !!entry.fun,
          prize: !!entry.prize,
        };
      }
      // Even with no entry (agent not in payload) we mark ready so the
      // buttons can reflect "all false" instead of staying in limbo.
      availabilityReady = true;
    } catch {
      // Keep defaults (all false) — buttons stay disabled; backend errors
      // surface on click. Still mark ready so we don't block the UI.
      availabilityReady = true;
    }
  }

  function flashRunToast(kind: "info" | "error" | "success", msg: string, ms = 5000) {
    runToast = { kind, msg };
    if (runToastTimer) clearTimeout(runToastTimer);
    runToastTimer = setTimeout(() => { runToast = null; runToastTimer = null; }, ms);
  }

  async function runSkill(tag: string) {
    if (idleRunningTag) return;
    idleRunningTag = tag;
    try {
      const result = await invoke<{ session_id?: string; skill_name?: string; tag?: string }>(
        "idle_run",
        { tag, agentKey, background: true },
      );
      if (result?.session_id) {
        backgroundIdleSessions = new Set([...backgroundIdleSessions, result.session_id]);
        backgroundSessionTags = new Map(backgroundSessionTags).set(result.session_id, skillLabel(tag));
        // Once it's running it will appear under "Active" via normal polling.
        flashRunToast("info", t("home.skill_started").replace("{label}", skillLabel(tag)), 4000);
      } else {
        flashRunToast("error", t("home.skill_run_failed").replace("{tag}", tag).replace("{err}", "no session"));
      }
    } catch (e) {
      const err = String(e);
      if (err.includes(t("chat.execute_failed"))) {
        flashRunToast("error", t("chat.execute_failed"));
      } else {
        flashRunToast("error", t("home.skill_run_failed").replace("{tag}", tag).replace("{err}", err));
      }
      idleRunningTag = null;
    }
  }

  // Release the run buttons once all background idle-runs have settled.
  function maybeReleaseRunButtons() {
    if (idleRunningTag && backgroundIdleSessions.size === 0) {
      idleRunningTag = null;
    }
  }

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
    // Refresh idle-run button availability on the same cadence so a change
    // in work-item queue / skill install is reflected without reload.
    await loadIdleAvailability();
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

    const et = data.event_type ?? "";

    // ── Background idle-run sessions (manual skill triggers) ────
    // Unified path for skills launched from this Home tab: track their
    // lifecycle and surface toast feedback without switching any view.
    // Automatic boredom-driven runs also carry background: true.
    if (et === "MessageReceived" && data?.background === true) {
      backgroundIdleSessions = new Set([...backgroundIdleSessions, data.session_id]);
      const rawTag: string = data.tag ?? "";
      backgroundSessionTags = new Map(backgroundSessionTags).set(
        data.session_id, rawTag ? skillLabel(rawTag) : "Idle",
      );
      flashRunToast("info", t("home.skill_started").replace("{label}", skillLabel(rawTag)), 4000);
    }
    if (backgroundIdleSessions.has(data.session_id)) {
      const tagLabel = backgroundSessionTags.get(data.session_id) ?? "Idle";
      if (et === "agent:awaiting_detach") {
        flashRunToast("info", t("home.skill_running").replace("{label}", tagLabel), 6000);
      } else if (et === "agent:reply_ready" || et === "agent:reply_interrupted") {
        backgroundIdleSessions = new Set([...backgroundIdleSessions].filter(s => s !== data.session_id));
        backgroundSessionTags = new Map([...backgroundSessionTags].filter(([s]) => s !== data.session_id));
        flashRunToast("success", t("home.skill_completed").replace("{label}", tagLabel), 5000);
        maybeReleaseRunButtons();
      } else if (et === "agent:reply_stream_error" || et === "llm_error") {
        backgroundIdleSessions = new Set([...backgroundIdleSessions].filter(s => s !== data.session_id));
        backgroundSessionTags = new Map([...backgroundSessionTags].filter(([s]) => s !== data.session_id));
        flashRunToast("error", t("home.skill_failed").replace("{label}", tagLabel), 5000);
        maybeReleaseRunButtons();
      }
    }

    // Refresh on any session-relevant event.
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
    await Promise.all([pollProcessingSessions(), loadWorkflows(), loadIdleAvailability()]);
    loading = false;

    // Poll every 5 s to catch processing state changes the SSE might miss.
    pollTimer = setInterval(() => {
      void pollProcessingSessions();
      void loadWorkflows();
    }, 5000);

    const unlisten = await listen("event:processed", handleEventProcessed);
    unlisteners.push(unlisten);

    // Live child-PID map per session, from the 2s agent_states snapshot.
    // running_children is only populated for the agent's active session, so
    // this stays small. Clear stale entries for sessions we no longer see.
    unlisteners.push(await listen("agent_states:updated", (e: any) => {
      const list: Array<{
        agent_id: string;
        active_session_id?: string | null;
        running_children?: number[];
      }> = e.payload?.agents ?? [];
      const next: Record<string, number[]> = {};
      for (const a of list) {
        if (a.agent_id !== agentKey) continue;
        if (a.active_session_id && a.running_children?.length) {
          next[a.active_session_id] = a.running_children;
        }
      }
      pidsBySession = next;
    }));
  });

  onDestroy(() => {
    for (const u of unlisteners) u();
    unlisteners = [];
    if (pollTimer) clearInterval(pollTimer);
    if (runToastTimer) clearTimeout(runToastTimer);
  });

  // Stamp live child Pids onto each session so the template can render them
  // without extra lookups. Recomputed whenever either source changes.
  const sessionsWithPids = $derived(
    sessions.map(s => ({ ...s, pids: pidsBySession[s.id] }))
  );
  const activeSessions = $derived(sessionsWithPids.filter(s => s.isProcessing));
  const idleSessions = $derived(sessionsWithPids.filter(s => !s.isProcessing));
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
      <div class="skill-quickrun">
        <span class="skill-quickrun-label">{t("home.run_skill")}</span>
        <div class="skill-buttons">
          {#each SKILL_TAGS as skill (skill.tag)}
            <button
              class="skill-btn"
              class:running={idleRunningTag === skill.tag}
              onclick={() => runSkill(skill.tag)}
              disabled={!availabilityReady || idleRunningTag !== null || !idleAvailability[skill.tag]}
              title={t("home.run_skill_hint").replace("{agent}", agentKey)}
            >
              <span class="skill-ico">{skill.icon}</span>
              <span class="skill-name">{skill.labelKey ? t(skill.labelKey) : skill.tag}</span>
              {#if idleRunningTag === skill.tag}<span class="skill-spin"></span>{/if}
            </button>
          {/each}
        </div>
      </div>
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
                    {#if session.pids?.length}
                      <span class="task-pids" title="Detached child processes">· PID {session.pids.join(', ')}</span>
                    {/if}
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

  {#if runToast}
    <div class="home-toast" class:info={runToast.kind === "info"} class:error={runToast.kind === "error"} class:success={runToast.kind === "success"}>
      {runToast.msg}
    </div>
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

  .task-pids {
    font-family: var(--font-mono, ui-monospace, monospace);
    color: #f59e0b;
    opacity: 0.9;
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

  /* ── Skill quick-run (idle empty-state) ──────────────────────── */

  .skill-quickrun {
    margin-top: 22px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
  }

  .skill-quickrun-label {
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-dim, #9ca3af);
  }

  .skill-buttons {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    justify-content: center;
  }

  .skill-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 8px 14px;
    font-size: 13px;
    font-weight: 500;
    color: var(--fg, #e5e7eb);
    background: var(--bg-card, rgba(255, 255, 255, 0.04));
    border: 1px solid var(--border, rgba(255, 255, 255, 0.08));
    border-radius: 9px;
    cursor: pointer;
    transition: background 0.15s, border-color 0.15s, transform 0.08s;
  }

  .skill-btn:hover:not(:disabled) {
    background: var(--bg-hover, rgba(255, 255, 255, 0.08));
    border-color: var(--accent, #6366f1);
  }

  .skill-btn:active:not(:disabled) {
    transform: translateY(1px);
  }

  .skill-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .skill-btn.running {
    border-color: var(--green, #22c55e);
    background: color-mix(in srgb, var(--green, #22c55e) 10%, transparent);
  }

  .skill-ico {
    font-size: 15px;
    line-height: 1;
  }

  .skill-name {
    white-space: nowrap;
  }

  .skill-spin {
    width: 11px;
    height: 11px;
    margin-left: 2px;
    border: 2px solid color-mix(in srgb, var(--green, #22c55e) 30%, transparent);
    border-top-color: var(--green, #22c55e);
    border-radius: 50%;
    animation: spin 0.7s linear infinite;
  }

  /* ── Run toast ────────────────────────────────────────────────── */

  .home-toast {
    position: fixed;
    bottom: 28px;
    left: 50%;
    transform: translateX(-50%);
    padding: 10px 18px;
    font-size: 13px;
    font-weight: 500;
    border-radius: 10px;
    background: var(--bg-card, rgba(255, 255, 255, 0.06));
    border: 1px solid var(--border, rgba(255, 255, 255, 0.1));
    color: var(--fg, #e5e7eb);
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.35);
    z-index: 50;
    max-width: 80vw;
    text-align: center;
    animation: toast-in 0.18s ease-out;
  }

  .home-toast.info {
    border-color: color-mix(in srgb, var(--accent, #6366f1) 50%, transparent);
  }

  .home-toast.success {
    border-color: color-mix(in srgb, var(--green, #22c55e) 50%, transparent);
    color: var(--green, #22c55e);
  }

  .home-toast.error {
    border-color: rgba(239, 68, 68, 0.5);
    color: #ef4444;
  }

  @keyframes toast-in {
    from { opacity: 0; transform: translateX(-50%) translateY(8px); }
    to   { opacity: 1; transform: translateX(-50%) translateY(0); }
  }
</style>
