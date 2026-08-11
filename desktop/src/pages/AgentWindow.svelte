<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import ActivityStateWidget from "./ActivityStateWidget.svelte";
  import ParticleField from "../components/ParticleField.svelte";
  import AgentHomeTab from "./AgentHomeTab.svelte";
  import AgentChatTab from "./AgentChatTab.svelte";
  import AgentContextTab from "./AgentContextTab.svelte";
  import { setAgentContext } from "../lib/agent-context.svelte";

  const { agentKey }: { agentKey: string } = $props();

  type TabId = "chat" | "context" | "home";
  let activeTab: TabId = $state("chat");

  let displayName = $state("");
  let hasProvider = $state(false);
  let unlisteners: (() => void) = [];

  // ── Idle system focus-driven control ──────────────────────────────
  // 窗体失焦后 12s 计时器，到期且 agent 非 Busy 时启动 idle system。
  // 窗体重新获焦时取消计时器并停止 idle system。
  const IDLE_START_DELAY_MS = 12_000;
  let idleStartTimer: ReturnType<typeof setTimeout> | null = null;
  let isFocused = $state(true);

  function clearIdleStartTimer() {
    if (idleStartTimer !== null) {
      clearTimeout(idleStartTimer);
      idleStartTimer = null;
    }
  }

  async function handleFocus() {
    if (isFocused) return; // 防止重复触发
    isFocused = true;
    clearIdleStartTimer();
    console.log(`[${agentKey}] window focused → stop idle`);
    // 窗体获焦 → 停止 idle system，进入 Ready 状态。
    try {
      await invoke("stop_agent_idle", { agentKey });
    } catch (e) {
      console.warn("stop_agent_idle failed:", e);
    }
  }

  async function handleBlur() {
    if (!isFocused) return; // 防止重复触发
    isFocused = false;
    console.log(`[${agentKey}] window blurred → starting ${IDLE_START_DELAY_MS}ms timer`);
    // 窗体失焦 → 启动 12s 计时器，到期后启动 idle system。
    clearIdleStartTimer();
    idleStartTimer = setTimeout(async () => {
      idleStartTimer = null;
      console.log(`[${agentKey}] timer expired → start idle`);
      try {
        await invoke("start_agent_idle", { agentKey });
      } catch (e) {
        // agent 可能处于 Busy 状态（有活跃 session），此时后端会拒绝启动。
        console.warn("start_agent_idle failed:", e);
      }
    }, IDLE_START_DELAY_MS);
  }

  async function loadIdentity() {
    try {
      const cfg: any = await invoke("get_aman_config");
      const entry = cfg?.agents?.[agentKey];
      if (entry?.display_name) displayName = entry.display_name;
      hasProvider = !!(entry?.provider);
    } catch { /* keep default */ }
    if (!displayName) displayName = agentKey;
  }

  onMount(async () => {
    await loadIdentity();

    const onSwitchTab = (e: Event) => {
      const tab = (e as CustomEvent).detail;
      if (tab === "chat" || tab === "context" || tab === "home") activeTab = tab;
    };
    window.addEventListener("agent-window:switch-tab", onSwitchTab);
    unlisteners.push(() => window.removeEventListener("agent-window:switch-tab", onSwitchTab));

    // 通过 Tauri 自定义事件监听窗体焦点变化（窗体级别，比 DOM focus/blur 可靠）。
    // 事件由后端 open_or_focus_agent_window 命令中的 on_focus_change 注册。
    const { listen } = await import("@tauri-apps/api/event");
    const unlistenFocused = await listen<{ agent_key: string }>(
      "agent-window:focused",
      (e) => {
        if (e.payload?.agent_key === agentKey) handleFocus();
      },
    );
    const unlistenBlurred = await listen<{ agent_key: string }>(
      "agent-window:blurred",
      (e) => {
        if (e.payload?.agent_key === agentKey) handleBlur();
      },
    );
    unlisteners.push(unlistenFocused, unlistenBlurred);

    // Capture agent:context_ready snapshots into the shared store (the
    // Context tab reads it). Registered at window level so it stays live
    // no matter which tab is active — no polling.
    const unlistenCtx = await listen("event:processed", (e: any) => {
      const payload = e.payload;
      if (!payload || payload.event_type !== "agent:context_ready") return;
      const data = payload.payload ?? {};
      const eventAgentId = data.agent_id ?? data.payload?.agent_id;
      if (eventAgentId && eventAgentId !== agentKey) return;
      setAgentContext(agentKey, data);
    });
    unlisteners.push(unlistenCtx);
  });

  onDestroy(() => {
    clearIdleStartTimer();
    for (const u of unlisteners) u();
    unlisteners = [];
  });
</script>

<!-- Layer 0: deep aurora-like animated gradient backdrop -->
<div class="aw-aurora"></div>

<!-- Layer 1: drifting particles -->
<div class="aw-particles">
  <ParticleField />
</div>

<!-- Layer 2: frosted glass UI -->
<div class="agent-window">
  <!-- Left: avatar with thick glass card -->
  <aside class="avatar-col glass-deep">
    <div class="avatar-inner">
      <ActivityStateWidget
        agentId={agentKey}
        agentName={displayName}
        runtimeRunning={true}
        visible={true}
        compact={false}
      />
    </div>
  </aside>

  <!-- Right: tabs + content with mid glass -->
  <section class="tabs-col glass-mid">
    <nav class="tab-bar">
      <button class="tab" class:active={activeTab === "chat"} onclick={() => activeTab = "chat"}>
        Chat
      </button>
      <button class="tab" class:active={activeTab === "context"} onclick={() => activeTab = "context"}>
        Context
      </button>
      <button class="tab" class:active={activeTab === "home"} onclick={() => activeTab = "home"}>
        Home
      </button>
    </nav>

    <div class="tab-content">
      {#if activeTab === "chat"}
        <AgentChatTab {agentKey} />
      {:else if activeTab === "context"}
        <AgentContextTab {agentKey} />
      {:else if activeTab === "home"}
        <AgentHomeTab {agentKey} />
      {/if}
    </div>
  </section>
</div>

<style>
  /* ── Override global #app styles ──────────────────────────────── */
  :global(#app) {
    padding-top: 0 !important;
  }
  :global(body) {
    background: #070a10 !important;
  }

  /* ── Layer 0: Aurora-like animated gradient ───────────────────────
   * Recreates the aurora feel without native vibrancy:
   * multiple radial gradients in green/teal/indigo that slowly drift. */
  .aw-aurora {
    position: fixed;
    inset: 0;
    z-index: 0;
    pointer-events: none;
    background:
      radial-gradient(ellipse 80% 60% at 15% 30%, rgba(13, 74, 26, 0.35) 0%, transparent 60%),
      radial-gradient(ellipse 70% 50% at 85% 70%, rgba(13, 51, 74, 0.30) 0%, transparent 55%),
      radial-gradient(ellipse 60% 80% at 50% 100%, rgba(45, 13, 160, 0.20) 0%, transparent 50%),
      radial-gradient(ellipse 90% 70% at 70% 20%, rgba(13, 26, 60, 0.40) 0%, transparent 50%);
    animation: auroraDrift 20s ease-in-out infinite alternate;
  }

  @keyframes auroraDrift {
    0% {
      transform: translate(0, 0) scale(1);
      filter: hue-rotate(0deg);
    }
    50% {
      transform: translate(2%, -1%) scale(1.05);
      filter: hue-rotate(15deg);
    }
    100% {
      transform: translate(-1%, 1%) scale(1.02);
      filter: hue-rotate(-10deg);
    }
  }

  /* ── Layer 1: Particles ─────────────────────────────────────────── */
  .aw-particles {
    position: fixed;
    inset: 0;
    z-index: 1;
    pointer-events: none;
  }

  /* ── Layer 2: Main layout ──────────────────────────────────────── */
  .agent-window {
    display: flex;
    width: 100%;
    height: 100vh;
    min-height: 0;
    overflow: hidden;
    position: relative;
    z-index: 2;
  }

  /* Glass depth classes — matching aman's frosted-glass hierarchy */
  .glass-deep {
    background: rgba(17, 20, 28, 0.35);
    border: 1px solid rgba(255, 255, 255, 0.07);
    backdrop-filter: blur(28px) saturate(1.4);
    -webkit-backdrop-filter: blur(28px) saturate(1.4);
  }

  .glass-mid {
    background: rgba(11, 13, 19, 0.52);
    border: 1px solid rgba(255, 255, 255, 0.06);
    backdrop-filter: blur(12px) saturate(1.2);
    -webkit-backdrop-filter: blur(12px) saturate(1.2);
  }

  /* ── Left: avatar column ───────────────────────────────────────── */
  .avatar-col {
    width: 240px;
    min-width: 210px;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border-right: 1px solid rgba(255, 255, 255, 0.06);
    position: relative;
    overflow: hidden;
  }

  /* Subtle top-glow accent strip inside the glass */
  .avatar-col::before {
    content: "";
    position: absolute;
    top: 0;
    left: 10%;
    right: 10%;
    height: 2px;
    background: linear-gradient(90deg,
      transparent 0%,
      rgba(91, 125, 245, 0.6) 40%,
      rgba(61, 191, 110, 0.4) 60%,
      transparent 100%);
    border-radius: 0 0 50% 50%;
  }

  .avatar-inner {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
  }

  /* Remove the horizontal separator that ActivityStateWidget draws
     (designed for the bottom-of-sidebar placement in the main window). */
  .avatar-inner :global(.activity-widget) {
    border-top: none !important;
    margin-top: 0 !important; /* override sidebar's margin-top: auto */
    align-items: center;      /* center children (name/ring/label) in the column */
  }
  .avatar-inner :global(.activity-widget .aw-name) {
    display: none;
  }
  .avatar-inner :global(.activity-widget .aw-state-label) {
    text-align: center;
  }

  /* ── Right: tabs column ────────────────────────────────────────── */
  .tabs-col {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-width: 0;
    border-left: none;
  }

  .tab-bar {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 10px 16px 0;
    border-bottom: 1px solid rgba(255, 255, 255, 0.07);
    flex-shrink: 0;
  }

  .tab {
    padding: 9px 24px;
    font-size: 13px;
    font-weight: 600;
    color: var(--fg-dim, #6b6e80);
    background: transparent;
    border: none;
    border-bottom: 2px solid transparent;
    cursor: pointer;
    transition: color 0.2s, border-color 0.2s;
    letter-spacing: 0.01em;
  }

  .tab:hover {
    color: var(--fg, #e4e6f0);
  }

  .tab.active {
    color: var(--fg, #e4e6f0);
    border-bottom-color: var(--accent, #5b73f5);
  }

  .tab-content {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }

  @media (prefers-reduced-motion: reduce) {
    .aw-aurora {
      animation: none;
    }
    .tab {
      transition: none;
    }
  }
</style>
