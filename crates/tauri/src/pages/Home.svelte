<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";
  import IdleRing from "./IdleRing.svelte";
  import AgentSelector from "./AgentSelector.svelte";
  import claudeIcon from "../lib/assets/code-agents/claude.svg?raw";
  import codexIcon from "../lib/assets/code-agents/codex.svg?raw";
  import opencodeIcon from "../lib/assets/code-agents/opencode.svg?raw";
  import geminiIcon from "../lib/assets/code-agents/gemini.svg?raw";

  const CODE_AGENT_ICONS: Record<string, string> = {
    "claude-code": claudeIcon,
    "codex": codexIcon,
    "opencode": opencodeIcon,
    "gemini-cli": geminiIcon,
  };

  let {
    onNavigate = (_page: string) => {},
    onNavigateChatWithSkill = async (_agentKey: string, _skillName: string) => {},
  }: {
    onNavigate?: (page: string) => void;
    onNavigateChatWithSkill?: (agentKey: string, skillName: string) => Promise<void>;
  } = $props();

  interface AgentEntry {
    key: string;
    display_name: string;
    provider: string;
    model: string;
    soul_summary: string;
    session_count: number;
    is_active: boolean;
  }

  interface CodeAgentEntry {
    key: string;
    display_name: string;
    command: string;
    description: string;
    available: boolean;
  }

  type Mode = "idle" | "reflection" | "processing";

  interface AgentIdleState {
    mode: Mode;
    outerPct: number;
    innerPct: number;
    emoji: string;
  }

  const COLORS: Record<Mode, { outer: string; inner: string }> = {
    idle:       { outer: "#6c8cff", inner: "#f59e0b" },
    reflection: { outer: "#a78bfa", inner: "#f472b6" },
    processing: { outer: "#4ade80", inner: "#22d3ee" },
  };

  const IDLE_EMOJI: Record<string, string> = {
    daze: "\u{1F636}", boredom: "\u{1F612}", sleep: "\u{1F634}",
    exploration: "\u{1F50D}", meditation: "\u{1F9D8}",
    incubation: "\u{1F4A1}", waiting: "\u{23F3}",
  };

  const MODE_ICON: Record<Mode, string> = {
    idle: "\u{1F4A4}", reflection: "\u{1F9E0}", processing: "\u{26A1}",
  };

  const THRESHOLDS = [0, 5, 20, 50, 100, 200];

  function depthPct(depth: number): number {
    if (depth <= 0) return 0;
    let idx = 0;
    for (let i = THRESHOLDS.length - 1; i >= 0; i--) {
      if (THRESHOLDS[i] <= depth) { idx = i; break; }
    }
    if (idx >= THRESHOLDS.length - 1) return 100;
    const cur = THRESHOLDS[idx];
    const next = THRESHOLDS[idx + 1];
    return Math.min(100, ((depth - cur) / (next - cur)) * 100);
  }

  function defaultIdleState(): AgentIdleState {
    return { mode: "idle", outerPct: 0, innerPct: 0, emoji: MODE_ICON.idle };
  }

  let agents = $state<AgentEntry[]>([]);
  let codeAgents = $state<CodeAgentEntry[]>([]);
  let loading = $state(true);
  let activeTab = $state<"agents" | "finance">("agents");
  let idleStates = $state<Record<string, AgentIdleState>>({});
  let unlisteners: (() => void)[] = [];
  let showAgentSelector = $state(false);
  let selectedSkillName = $state("");
  let modalError = $state("");

  function ensureIdleState(key: string) {
    if (!(key in idleStates)) {
      idleStates = { ...idleStates, [key]: defaultIdleState() };
    }
  }

  function handleIdleEvent(e: any) {
    const p = e.payload;
    if (!p?.event_type) return;
    const et: string = p.event_type;
    const data = p.payload ?? {};
    const agentId: string | undefined = data.agent_id;

    if (!agentId) return;
    ensureIdleState(agentId);

    if (et === "idle") {
      const depth: number = data.depth ?? 0;
      const arousal: number = data.context?.arousal_level ?? 0.5;
      const kind: string = data.kind ?? "daze";
      idleStates = {
        ...idleStates,
        [agentId]: {
          mode: "idle",
          outerPct: depthPct(depth),
          innerPct: Math.round(arousal * 100),
          emoji: IDLE_EMOJI[kind] ?? MODE_ICON.idle,
        },
      };
    } else if (et === "agent:reply_stream_start" || et === "agent:reply_chunk" ||
               et === "agent:reply_stream_done" || et === "agent:reply_ready" ||
               et === "tool:dispatched" || et === "tool:completed" || et === "tool:failed") {
      // Agent is active — show processing
      idleStates = {
        ...idleStates,
        [agentId]: { mode: "processing", outerPct: 50, innerPct: 50, emoji: MODE_ICON.processing },
      };
    }
  }

  function getIdleState(key: string): AgentIdleState {
    return idleStates[key] ?? defaultIdleState();
  }

  async function selectAgent(agent: AgentEntry) {
    if (!agent.provider) {
      onNavigate("agents");
      return;
    }
    try {
      await invoke("select_agent", { key: agent.key });
      onNavigate("chat");
    } catch {
      // silent
    }
  }

  async function launchCodeAgent(ca: CodeAgentEntry) {
    if (!ca.available) return;
    try {
      await invoke("launch_code_agent", { command: ca.command });
    } catch (e) {
      if (String(e) !== "CANCELLED") {
        console.error("launch_code_agent failed:", e);
      }
    }
  }

  function onFinanceClick(skillName: string) {
    selectedSkillName = skillName;
    modalError = "";
    showAgentSelector = true;
  }

  async function handleFinanceAgentSelect(agent: AgentEntry) {
    if (!agent.provider) {
      showAgentSelector = false;
      onNavigate("agents");
      return;
    }
    try {
      await invoke("select_agent", { key: agent.key });
      showAgentSelector = false;
      await onNavigateChatWithSkill(agent.key, selectedSkillName);
    } catch (e) {
      modalError = String(e);
    }
  }

  onMount(async () => {
    try {
      agents = await invoke<AgentEntry[]>("list_agents");
      // Initialize default idle state for each agent
      const init: Record<string, AgentIdleState> = {};
      for (const a of agents) {
        init[a.key] = defaultIdleState();
      }
      idleStates = init;
    } catch {
      // no agents or config missing
    } finally {
      loading = false;
    }

    try {
      codeAgents = await invoke<CodeAgentEntry[]>("list_code_agents");
    } catch {
      // code agents file missing or unparseable
    }

    unlisteners.push(await listen("event:processed", handleIdleEvent));
  });

  onDestroy(() => {
    for (const fn of unlisteners) fn();
  });
</script>

<div class="home-tabs">
  <button
    class="home-tab"
    class:active={activeTab === "agents"}
    onclick={() => (activeTab = "agents")}
  >
    Agents
  </button>
  <button
    class="home-tab"
    class:active={activeTab === "finance"}
    onclick={() => (activeTab = "finance")}
  >
    Finance
  </button>
</div>

{#if activeTab === "agents"}
  <div class="home-section">
    {#if loading}
      <p class="dim" style="text-align:center;padding:40px;">Loading...</p>
    {:else if agents.length === 0}
      <div class="card" style="text-align:center;padding:40px;">
        <p>No agents yet.</p>
        <p class="dim" style="margin-top:8px;">Go to Services → Agents to create your first agent.</p>
      </div>
    {:else}
      <div class="agent-grid">
        {#each agents as agent}
          {@const st = getIdleState(agent.key)}
          <button class="agent-avatar-card" class:needs-config={!agent.provider} onclick={() => selectAgent(agent)}>
            <IdleRing
              mode={st.mode}
              outerPct={st.outerPct}
              innerPct={st.innerPct}
              emoji={st.emoji}
              ringColors={COLORS[st.mode]}
              size={56}
              showLabel={false}
              showInfo={false}
              active={!!agent.provider}
            />
            <span class="agent-avatar-name">{agent.display_name}</span>
            {#if !agent.provider}
              <span class="badge warn" style="margin-top:2px;font-size:10px;">needs config</span>
            {:else if agent.is_active}
              <span class="badge ok" style="margin-top:2px;font-size:10px;">active</span>
            {/if}
          </button>
        {/each}
      </div>

      {#if codeAgents.length > 0}
        <hr class="section-divider" />
        <h3 class="section-label">Code Agents</h3>
        <div class="code-agent-grid">
          {#each codeAgents as ca}
            <button
              class="code-agent-card"
              class:unavailable={!ca.available}
              onclick={() => launchCodeAgent(ca)}
            >
              <div class="code-agent-icon">{@html CODE_AGENT_ICONS[ca.key] || ""}</div>
              <span class="agent-avatar-name">{ca.display_name}</span>
              {#if ca.available}
                <span class="badge ok" style="margin-top:2px;font-size:10px;">available</span>
              {:else}
                <span class="badge dim" style="margin-top:2px;font-size:10px;">not found</span>
              {/if}
            </button>
          {/each}
        </div>
      {/if}
    {/if}
  </div>
{:else}
  <div class="home-section">
    <div class="finance-cards">
      <button class="finance-card" onclick={() => onFinanceClick("ipo-research")}>
        <div class="finance-card-accent"></div>
        <div class="finance-card-icon"><span>📊</span></div>
        <div class="finance-card-body">
          <h3>股票打新</h3>
          <p class="dim">新股申购分析与研究</p>
          <div class="finance-card-footer">
            <span class="finance-skill-tag">ipo-research</span>
            <span class="finance-card-arrow">→</span>
          </div>
        </div>
      </button>
      <button class="finance-card" onclick={() => onFinanceClick("unlisted-ecosystem-analysis")}>
        <div class="finance-card-accent"></div>
        <div class="finance-card-icon"><span>🔍</span></div>
        <div class="finance-card-body">
          <h3>未上市公司调研</h3>
          <p class="dim">非上市公司生态分析</p>
          <div class="finance-card-footer">
            <span class="finance-skill-tag">unlisted-ecosystem-analysis</span>
            <span class="finance-card-arrow">→</span>
          </div>
        </div>
      </button>
    </div>
  </div>
{/if}

{#if showAgentSelector}
  <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
  <div class="modal-overlay" onclick={() => showAgentSelector = false} onkeydown={() => {}} role="button" tabindex="0">
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
    <div class="modal-content" onclick={(e) => e.stopPropagation()} onkeydown={() => {}} role="dialog" tabindex="-1">
      <div class="modal-header">
        <h3>选择 Agent 执行 "{selectedSkillName}"</h3>
        <button class="modal-close-btn" onclick={() => showAgentSelector = false}>✕</button>
      </div>
      {#if modalError}
        <div class="toast toast-error">{modalError}</div>
      {/if}
      <AgentSelector
        agents={agents}
        variant="compact"
        onSelect={handleFinanceAgentSelect}
      />
    </div>
  </div>
{/if}

<style>
  .home-tabs {
    display: flex;
    gap: 4px;
    margin-bottom: 24px;
    border-bottom: 1px solid var(--border);
  }
  .home-tab {
    padding: 10px 20px;
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--fg-dim);
    font-size: 14px;
    font-weight: 500;
    cursor: pointer;
    transition: color 0.15s, border-color 0.15s;
    border-radius: 0;
    margin-bottom: -1px;
  }
  .home-tab:hover {
    color: var(--fg);
    background: none;
  }
  .home-tab.active {
    color: var(--fg);
    border-bottom-color: var(--accent);
  }

  .home-section {
    /* spacer */
  }

  .agent-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 16px;
  }

  .agent-avatar-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    padding: 20px 16px 16px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 12px;
    cursor: pointer;
    transition: border-color 0.2s, transform 0.15s;
    text-align: center;
  }
  .agent-avatar-card:hover {
    border-color: var(--accent);
    transform: translateY(-2px);
  }

  .agent-avatar-card.needs-config {
    opacity: 0.5;
    cursor: pointer;
  }

  .agent-avatar-card.needs-config:hover {
    opacity: 0.7;
    border-color: var(--warn, #f59e0b);
  }

  .agent-avatar-name {
    font-size: 14px;
    font-weight: 600;
    color: var(--fg);
    line-height: 1.3;
    word-break: break-word;
  }

  /* Finance cards */
  .finance-cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 20px;
  }

  .finance-card {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: 18px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 14px;
    padding: 24px;
    cursor: pointer;
    text-align: left;
    font-family: inherit;
    color: inherit;
    width: 100%;
    overflow: hidden;
    transition: border-color 0.25s, transform 0.2s, box-shadow 0.2s;
  }
  .finance-card:hover {
    border-color: var(--accent);
    transform: translateY(-2px);
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.2);
  }
  .finance-card:active {
    transform: translateY(0);
  }

  /* Colored accent stripe at top */
  .finance-card-accent {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 3px;
    background: linear-gradient(90deg, var(--accent, #6c8cff), #a78bfa);
    border-radius: 14px 14px 0 0;
  }

  /* Icon in a rounded background */
  .finance-card-icon {
    flex-shrink: 0;
    width: 52px;
    height: 52px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: linear-gradient(135deg, rgba(108,140,255,0.12), rgba(167,139,250,0.08));
    border-radius: 14px;
    margin-top: 2px;
  }
  .finance-card-icon span {
    font-size: 26px;
    line-height: 1;
  }

  .finance-card-body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .finance-card-body h3 {
    font-size: 16px;
    font-weight: 600;
    margin: 0;
    color: var(--fg);
    line-height: 1.3;
  }
  .finance-card-body p {
    font-size: 13px;
    margin: 0;
    line-height: 1.5;
  }

  .finance-card-footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 8px;
  }
  .finance-skill-tag {
    display: inline-block;
    padding: 3px 10px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 4px;
    font-size: 11px;
    font-family: "SF Mono", "Fira Code", monospace;
    color: var(--fg-dim);
  }
  .finance-card-arrow {
    font-size: 16px;
    color: var(--fg-dim);
    transition: color 0.2s, transform 0.2s;
  }
  .finance-card:hover .finance-card-arrow {
    color: var(--accent);
    transform: translateX(3px);
  }

  .dim { color: var(--fg-dim); }

  /* Modal */
  .modal-overlay {
    position: fixed;
    inset: 0;
    z-index: 2000;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .modal-content {
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 24px;
    min-width: 400px;
    max-width: 700px;
    max-height: 80vh;
    overflow-y: auto;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
  }
  .modal-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 16px;
  }
  .modal-header h3 {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
  }
  .modal-close-btn {
    background: none;
    border: none;
    color: var(--fg-dim);
    font-size: 20px;
    cursor: pointer;
    padding: 4px 8px;
    border-radius: 4px;
  }
  .modal-close-btn:hover {
    background: var(--bg-hover);
    color: var(--fg);
  }
  .toast-error {
    background: rgba(248,113,113,0.15);
    color: var(--red);
    padding: 10px 16px;
    border-radius: 6px;
    margin-bottom: 16px;
    font-size: 13px;
  }

  /* Code agents section */
  .section-divider {
    margin: 28px 0 20px;
    border: none;
    border-top: 1px solid var(--border);
  }

  .section-label {
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--fg-dim);
    margin-bottom: 12px;
  }

  .code-agent-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 16px;
  }

  .code-agent-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    padding: 20px 16px 16px;
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 12px;
    cursor: pointer;
    transition: border-color 0.2s, transform 0.15s;
    text-align: center;
  }

  .code-agent-card:hover {
    border-color: var(--accent);
    transform: translateY(-2px);
  }

  .code-agent-card.unavailable {
    opacity: 0.45;
    cursor: default;
  }

  .code-agent-card.unavailable:hover {
    border-color: var(--border);
    transform: none;
  }

  .code-agent-icon {
    width: 56px;
    height: 56px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--bg);
    border-radius: 14px;
    font-size: 28px;
    line-height: 1;
    color: var(--fg);
  }
  .code-agent-icon :global(svg) {
    width: 28px;
    height: 28px;
  }

  .badge.dim {
    background: var(--bg-hover, rgba(255,255,255,0.06));
    color: var(--fg-dim);
    padding: 2px 8px;
    border-radius: 4px;
    font-weight: 500;
  }
</style>
