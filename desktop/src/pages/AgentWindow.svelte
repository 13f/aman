<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import ActivityStateWidget from "./ActivityStateWidget.svelte";
  import ParticleField from "../components/ParticleField.svelte";
  import AgentHomeTab from "./AgentHomeTab.svelte";
  import AgentChatTab from "./AgentChatTab.svelte";

  const { agentKey }: { agentKey: string } = $props();

  type TabId = "home" | "chat";
  let activeTab: TabId = $state("home");

  let displayName = $state("");
  let hasProvider = $state(false);
  let unlisteners: (() => void) = [];

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
      if (tab === "chat" || tab === "home") activeTab = tab;
    };
    window.addEventListener("agent-window:switch-tab", onSwitchTab);
    unlisteners.push(() => window.removeEventListener("agent-window:switch-tab", onSwitchTab));
  });

  onDestroy(() => {
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
      <button class="tab" class:active={activeTab === "home"} onclick={() => activeTab = "home"}>
        Home
      </button>
      <button class="tab" class:active={activeTab === "chat"} onclick={() => activeTab = "chat"}>
        Chat
      </button>
    </nav>

    <div class="tab-content">
      {#if activeTab === "home"}
        <AgentHomeTab {agentKey} />
      {:else if activeTab === "chat"}
        <AgentChatTab {agentKey} />
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
