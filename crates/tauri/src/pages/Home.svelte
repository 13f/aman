<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

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

  let agents = $state<AgentEntry[]>([]);
  let loading = $state(true);
  let activeTab = $state<"agents" | "finance">("agents");

  function avatarColor(name: string): string {
    let hash = 0;
    for (let i = 0; i < name.length; i++) {
      hash = name.charCodeAt(i) + ((hash << 5) - hash);
    }
    const hue = Math.abs(hash) % 360;
    return `hsl(${hue}, 50%, 42%)`;
  }

  function avatarInitials(name: string): string {
    const parts = name.trim().split(/\s+/);
    if (parts.length >= 2) {
      return (parts[0][0] + parts[1][0]).toUpperCase();
    }
    return name.slice(0, 2).toUpperCase();
  }

  async function selectAgent(key: string) {
    try {
      await invoke("select_agent", { key });
      onNavigate("chat");
    } catch {
      // agent selection failed silently
    }
  }

  onMount(async () => {
    try {
      agents = await invoke<AgentEntry[]>("list_agents");
    } catch {
      // no agents or config missing
    } finally {
      loading = false;
    }
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
          <button class="agent-avatar-card" onclick={() => selectAgent(agent.key)}>
            <div
              class="agent-avatar"
              style="background: {avatarColor(agent.display_name)}"
            >
              <span class="agent-avatar-initials">{avatarInitials(agent.display_name)}</span>
            </div>
            <span class="agent-avatar-name">{agent.display_name}</span>
            {#if agent.is_active}
              <span class="badge ok" style="margin-top:4px;font-size:10px;">active</span>
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
    padding: 24px 16px 20px;
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

  .agent-avatar {
    width: 72px;
    height: 72px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }

  .agent-avatar-initials {
    color: #fff;
    font-size: 24px;
    font-weight: 700;
    letter-spacing: 0.5px;
    user-select: none;
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
