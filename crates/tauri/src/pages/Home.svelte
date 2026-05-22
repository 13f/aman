<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";
  import IdleRing from "./IdleRing.svelte";

  let { onNavigate = (_page: string) => {} }: { onNavigate?: (page: string) => void } = $props();

  interface AgentEntry {
    key: string;
    display_name: string;
    provider: string;
    model: string;
    soul_summary: string;
    session_count: number;
    is_active: boolean;
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
  let loading = $state(true);
  let activeTab = $state<"agents" | "finance">("agents");
  let idleStates = $state<Record<string, AgentIdleState>>({});
  let unlisteners: (() => void)[] = [];

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

  async function selectAgent(key: string) {
    try {
      await invoke("select_agent", { key });
      onNavigate("chat");
    } catch {
      // silent
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
          <button class="agent-avatar-card" onclick={() => selectAgent(agent.key)}>
            <IdleRing
              mode={st.mode}
              outerPct={st.outerPct}
              innerPct={st.innerPct}
              emoji={st.emoji}
              ringColors={COLORS[st.mode]}
              size={56}
              showLabel={false}
              showInfo={false}
            />
            <span class="agent-avatar-name">{agent.display_name}</span>
            {#if agent.is_active}
              <span class="badge ok" style="margin-top:2px;font-size:10px;">active</span>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>
{:else}
  <div class="home-section">
    <div class="finance-cards">
      <div class="finance-card">
        <div class="finance-card-icon">📊</div>
        <h3>股票打新</h3>
        <p class="dim">IPO Research — 新股申购分析与研究</p>
        <span class="finance-skill-tag">ipo-research</span>
      </div>
      <div class="finance-card">
        <div class="finance-card-icon">🔍</div>
        <h3>未上市公司调研</h3>
        <p class="dim">Unlisted Ecosystem Analysis — 非上市公司生态分析</p>
        <span class="finance-skill-tag">unlisted-ecosystem-analysis</span>
      </div>
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
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 16px;
  }

  .finance-card {
    background: var(--bg-card);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 24px;
    cursor: default;
    transition: border-color 0.2s;
  }
  .finance-card:hover {
    border-color: var(--accent);
  }
  .finance-card-icon {
    font-size: 32px;
    margin-bottom: 12px;
  }
  .finance-card h3 {
    font-size: 16px;
    font-weight: 600;
    margin-bottom: 6px;
    color: var(--fg);
  }
  .finance-card p {
    font-size: 13px;
    margin-bottom: 12px;
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
  .dim { color: var(--fg-dim); }
</style>
