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
  import kimiIcon from "../lib/assets/code-agents/kimi.svg?raw";
  import grokIcon from "../lib/assets/code-agents/grok.svg?raw";

  const CODE_AGENT_ICONS: Record<string, string> = {
    "claude-code": claudeIcon,
    "codex": codexIcon,
    "opencode": opencodeIcon,
    "gemini-cli": geminiIcon,
    "kimi": kimiIcon,
    "grok": grokIcon,
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

  interface FinanceCard {
    skill_name: string;
    title: string;
    subtitle: string;
    icon: string;
  }

  interface LlmSkill {
    name: string;
    description: string;
  }

  const ICON_EMOJI: Record<string, string> = {
    chart: "\u{1F4CA}", search: "\u{1F50D}", default: "\u{1F4CB}",
    code: "\u{1F4BB}", brain: "\u{1F9E0}", globe: "\u{1F310}",
    database: "\u{1F5C4}\u{FE0F}", shield: "\u{1F6E1}\u{FE0F}",
  };

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

  // System state visuals (used when NOT idle)
  const STATE_EMOJI: Record<string, string> = {
    working: "\u{1F6E0}\u{FE0F}",   // 🛠️ hammer & wrench
    studying: "\u{1F4DA}",          // 📚 books
    daily_life: "\u{1F3E0}",        // 🏠 house
  };
  const STATE_COLOR: Record<string, string> = {
    working: "#4ade80",
    studying: "#a78bfa",
    daily_life: "#fbbf24",
  };
  const STATE_ANIM: Record<string, string> = {
    working: "anim-spin-slow",
    studying: "anim-float",
    daily_life: "anim-pulse-soft",
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
  let systemStates = $state<Record<string, string>>({});
  let unlisteners: (() => void)[] = [];
  let showAgentSelector = $state(false);
  let selectedSkillName = $state("");
  let modalError = $state("");

  // Finance cards
  let financeCards = $state<FinanceCard[]>([]);
  let financeLoading = $state(true);
  let showAddSkill = $state(false);
  let addSkillError = $state("");
  let addSkillSearch = $state("");
  let llmSkills = $state<LlmSkill[]>([]);
  let llmSkillsLoading = $state(false);

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

  function getSystemState(key: string): string {
    return systemStates[key] ?? "idle";
  }

  const SYSTEM_STATE_LABEL: Record<string, string> = {
    idle: "Idle",
    working: "Working",
    chatting: "Chatting",
    studying: "Studying",
    daily_life: "Daily Life",
  };

  const SYSTEM_STATE_CLASS: Record<string, string> = {
    idle: "ss-idle",
    working: "ss-working",
    chatting: "ss-chatting",
    studying: "ss-studying",
    daily_life: "ss-dailylife",
  };

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

  function iconEmoji(icon: string): string {
    return ICON_EMOJI[icon] ?? ICON_EMOJI.default;
  }

  async function loadFinanceCards() {
    financeLoading = true;
    try {
      financeCards = await invoke<FinanceCard[]>("list_finance_cards");
    } catch {
      financeCards = [];
    } finally {
      financeLoading = false;
    }
  }

  async function removeCard(skillName: string) {
    try {
      await invoke("remove_finance_card", { skillName });
      financeCards = financeCards.filter(c => c.skill_name !== skillName);
    } catch (e) {
      console.error("remove_finance_card failed:", e);
    }
  }

  async function openAddSkill() {
    addSkillError = "";
    addSkillSearch = "";
    if (llmSkills.length === 0) {
      llmSkillsLoading = true;
      try {
        const v = await invoke<any>("list_llm_skills");
        const items = v?.items as Array<{ name: string; description: string }> | undefined;
        llmSkills = (items || []).filter(s => s.name && s.description);
      } catch {
        addSkillError = "无法加载技能列表，请确认 Gateway 已启动";
      } finally {
        llmSkillsLoading = false;
      }
    }
    showAddSkill = true;
  }

  function addCard(skill: LlmSkill) {
    const subtitle = skill.description.length > 60
      ? skill.description.slice(0, 60) + "..."
      : skill.description;
    invoke("add_finance_card", {
      skillName: skill.name,
      title: skill.name,
      subtitle,
      icon: "default",
    }).then(() => {
      loadFinanceCards();
    }).catch((e) => {
      addSkillError = String(e);
    });
    showAddSkill = false;
  }

  let filteredAddSkills = $derived(
    addSkillSearch.trim()
      ? llmSkills.filter(s => {
          const q = addSkillSearch.toLowerCase();
          return s.name.toLowerCase().includes(q) || s.description.toLowerCase().includes(q);
        })
      : llmSkills
  );

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

    loadFinanceCards();

    unlisteners.push(await listen("event:processed", handleIdleEvent));

    // Listen for system state updates from the gateway
    unlisteners.push(await listen("agent_states:updated", (e: any) => {
      const list: Array<{ agent_id: string; system_state: string }> = e.payload?.agents ?? [];
      const next: Record<string, string> = {};
      for (const a of list) {
        next[a.agent_id] = a.system_state;
      }
      systemStates = next;
    }));
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
          {@const ss = getSystemState(agent.key)}
          <button class="agent-avatar-card" class:needs-config={!agent.provider} onclick={() => selectAgent(agent)}>
            {#if ss === "idle" || !agent.provider}
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
            {:else}
              <div
                class="state-visual {STATE_ANIM[ss] ?? ''}"
                style="--st-color: {STATE_COLOR[ss] ?? '#6c8cff'}; width:56px; height:56px;"
              >
                <span class="state-emoji">{STATE_EMOJI[ss] ?? "\u{1F4CB}"}</span>
              </div>
            {/if}
            <span class="agent-avatar-name">{agent.display_name}</span>
            {#if !agent.provider}
              <span class="badge warn" style="margin-top:2px;font-size:10px;">needs config</span>
            {:else}
              <span class="system-state-badge {SYSTEM_STATE_CLASS[ss] ?? 'ss-idle'}">
                {SYSTEM_STATE_LABEL[ss] ?? ss}
              </span>
            {/if}
          </button>
        {/each}
      </div>

      {#if codeAgents.length > 0}
        <hr class="section-divider" />
        <h3 class="section-label">
          Code Agents
          <span class="terminal-hint" title="Opens a new terminal window for these code agents">
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <polyline points="4 17 10 11 4 5"></polyline>
              <line x1="12" y1="19" x2="20" y2="19"></line>
            </svg>
          </span>
        </h3>
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
    {#if financeLoading}
      <p class="dim" style="text-align:center;padding:40px;">Loading...</p>
    {:else}
      <div class="finance-cards">
        {#each financeCards as card (card.skill_name)}
          <button class="finance-card" onclick={() => onFinanceClick(card.skill_name)}>
            <div class="finance-card-accent"></div>
            <span
              class="finance-card-remove"
              title="移除卡片"
              role="button"
              tabindex="0"
              onclick={(e) => { e.stopPropagation(); removeCard(card.skill_name); }}
              onkeydown={(e) => { if (e.key === 'Enter') { e.stopPropagation(); removeCard(card.skill_name); } }}
            >&times;</span>
            <div class="finance-card-icon"><span>{iconEmoji(card.icon)}</span></div>
            <div class="finance-card-body">
              <h3>{card.title}</h3>
              <p class="dim">{card.subtitle}</p>
              <div class="finance-card-footer">
                <span class="finance-skill-tag">{card.skill_name}</span>
                <span class="finance-card-arrow">&rarr;</span>
              </div>
            </div>
          </button>
        {/each}
        <button class="finance-card finance-card-add" onclick={openAddSkill}>
          <div class="finance-card-icon finance-card-add-icon"><span>+</span></div>
          <div class="finance-card-body">
            <h3>添加技能卡片</h3>
            <p class="dim">从可用技能列表中选择</p>
          </div>
        </button>
      </div>
    {/if}
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

{#if showAddSkill}
  <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
  <div class="modal-overlay" onclick={() => showAddSkill = false} onkeydown={() => {}} role="button" tabindex="0">
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -->
    <div class="modal-content" onclick={(e) => e.stopPropagation()} onkeydown={() => {}} role="dialog" tabindex="-1">
      <div class="modal-header">
        <h3>添加技能卡片</h3>
        <button class="modal-close-btn" onclick={() => showAddSkill = false}>&times;</button>
      </div>
      {#if addSkillError}
        <div class="toast-error">{addSkillError}</div>
      {/if}
      {#if llmSkillsLoading}
        <p class="dim" style="text-align:center;padding:20px;">Loading...</p>
      {:else}
        <input
          type="text"
          class="add-skill-search"
          placeholder="搜索技能..."
          bind:value={addSkillSearch}
        />
        <div class="add-skill-list">
          {#each filteredAddSkills as skill (skill.name)}
            <button class="add-skill-item" onclick={() => addCard(skill)}>
              <div class="add-skill-item-body">
                <span class="add-skill-item-name">{skill.name}</span>
                <span class="add-skill-item-desc">{skill.description}</span>
              </div>
              <span class="add-skill-item-plus">+</span>
            </button>
          {:else}
            <p class="dim" style="text-align:center;padding:20px;">
              {addSkillSearch ? "无匹配技能" : "没有可用的技能"}
            </p>
          {/each}
        </div>
      {/if}
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
    opacity: 0.45;
    cursor: pointer;
  }

  .agent-avatar-card.needs-config:hover {
    opacity: 0.65;
    border-color: var(--yellow);
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
    box-shadow: var(--shadow-md);
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
    background: linear-gradient(90deg, var(--accent), var(--accent-hover));
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
    background: var(--accent-muted);
    border-radius: var(--radius-xl);
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
    box-shadow: var(--shadow-xl);
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
    background: var(--red-muted);
    color: var(--red);
    padding: 10px 16px;
    border-radius: var(--radius-md);
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
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .terminal-hint {
    display: inline-flex;
    align-items: center;
    color: var(--fg-dim);
    cursor: help;
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
    background: var(--bg-hover);
    color: var(--fg-dim);
    padding: 2px 8px;
    border-radius: var(--radius-sm);
    font-weight: 500;
  }

  /* Finance card remove button */
  .finance-card-remove {
    position: absolute;
    top: 8px;
    right: 10px;
    z-index: 2;
    background: none;
    border: none;
    color: var(--fg-dim);
    font-size: 18px;
    line-height: 1;
    cursor: pointer;
    padding: 2px 6px;
    border-radius: 4px;
    opacity: 0;
    transition: opacity 0.15s, color 0.15s, background 0.15s;
  }
  .finance-card:hover .finance-card-remove {
    opacity: 1;
  }
  .finance-card-remove:hover {
    color: var(--red, #f87171);
    background: rgba(248,113,113,0.12);
  }

  /* Add card button */
  .finance-card-add {
    border-style: dashed;
    opacity: 0.7;
  }
  .finance-card-add:hover {
    opacity: 1;
    border-style: dashed;
  }
  .finance-card-add-icon {
    background: var(--accent-muted);
    opacity: 0.6;
  }
  .finance-card-add-icon span {
    font-size: 28px;
    font-weight: 300;
    color: var(--fg-dim);
  }

  /* Add skill modal */
  .add-skill-search {
    width: 100%;
    padding: 10px 14px;
    margin-bottom: 16px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 8px;
    color: var(--fg);
    font-size: 14px;
    font-family: inherit;
    outline: none;
    box-sizing: border-box;
  }
  .add-skill-search:focus {
    border-color: var(--accent);
  }
  .add-skill-search::placeholder {
    color: var(--fg-dim);
  }

  .add-skill-list {
    max-height: 360px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .add-skill-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 14px;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 8px;
    cursor: pointer;
    text-align: left;
    font-family: inherit;
    color: inherit;
    width: 100%;
    transition: border-color 0.15s, background 0.15s;
  }
  .add-skill-item:hover {
    border-color: var(--accent);
    background: var(--bg-card);
  }
  .add-skill-item-body {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .add-skill-item-name {
    font-size: 14px;
    font-weight: 600;
    color: var(--fg);
  }
  .add-skill-item-desc {
    font-size: 12px;
    color: var(--fg-dim);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .add-skill-item-plus {
    flex-shrink: 0;
    margin-left: 12px;
    font-size: 20px;
    font-weight: 300;
    color: var(--fg-dim);
    width: 28px;
    height: 28px;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 6px;
    background: var(--accent-muted);
    transition: background 0.15s, color 0.15s;
  }
  .add-skill-item:hover .add-skill-item-plus {
    background: var(--accent);
    color: #fff;
  }

  /* System state badges */
  .system-state-badge {
    display: inline-block;
    padding: 2px 10px;
    border-radius: 10px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.02em;
    margin-top: 2px;
    transition: background 0.3s, color 0.3s;
  }
  .ss-idle {
    background: var(--accent-muted);
    color: var(--accent);
  }
  .ss-working {
    background: var(--green-muted);
    color: var(--green);
    animation: workingPulse 2s ease-in-out infinite;
  }
  .ss-studying {
    background: rgba(167, 139, 250, 0.12);
    color: #b39dfc;
  }
  .ss-dailylife {
    background: var(--yellow-muted);
    color: var(--yellow);
  }
  .ss-chatting {
    background: rgba(34, 211, 238, 0.12);
    color: #38dff0;
    animation: workingPulse 2s ease-in-out infinite;
  }

  @keyframes workingPulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.65; }
  }

  /* State visual — emoji circle for non-idle states */
  .state-visual {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    border-radius: 50%;
    background: color-mix(in srgb, var(--st-color, #6c8cff) 12%, transparent);
    border: 2px solid color-mix(in srgb, var(--st-color, #6c8cff) 35%, transparent);
    flex-shrink: 0;
  }
  .state-emoji {
    font-size: 24px;
    line-height: 1;
    user-select: none;
  }

  /* Animations for non-idle states */
  .anim-spin-slow {
    animation: spinSlow 4s linear infinite;
  }
  .anim-float {
    animation: float 3s ease-in-out infinite;
  }
  .anim-pulse-soft {
    animation: pulseSoft 2.5s ease-in-out infinite;
  }

  @keyframes spinSlow {
    from { transform: rotate(0deg); }
    to { transform: rotate(360deg); }
  }
  @keyframes float {
    0%, 100% { transform: translateY(0); }
    50% { transform: translateY(-4px); }
  }
  @keyframes pulseSoft {
    0%, 100% { transform: scale(1); }
    50% { transform: scale(1.08); }
  }
</style>
