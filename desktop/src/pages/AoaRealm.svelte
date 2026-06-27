<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { getTheme } from "../lib/aoa/theme/index";
  import type { HeroSnapshot } from "../lib/aoa/shared/index";
  import { useWorld } from "../lib/aoa/store";
  import type { AgentEntry, AgentIdleState } from "./agent-viewer-types";

  let {
    agents = [],
    idleStates = {} as Record<string, AgentIdleState>,
    systemStates = {} as Record<string, string>,
    onSelect = (_agent: AgentEntry) => {},
  }: {
    agents: AgentEntry[];
    idleStates?: Record<string, AgentIdleState>;
    systemStates?: Record<string, string>;
    onSelect?: (agent: AgentEntry) => void;
  } = $props();

  let hostEl: HTMLDivElement;
  let gameView: any = null;
  let status = $state("loading");

  // ── Aman agent → HeroSnapshot mapping ──────────────────────────────────

  function mapState(systemState: string, isActive: boolean): import("../lib/aoa/shared/index").HeroStateKind {
    if (!isActive) return "sleeping";
    switch (systemState) {
      case "working": return "working";
      case "chatting": return "working";
      case "studying": return "thinking";
      case "daily_life": return "idle";
      case "prize": return "idle";
      case "waiting": return "awaiting-input";
      default: return "idle";
    }
  }

  function pushHeroes() {
    if (agents.length === 0) return;
    const now = new Date().toISOString();
    const heroes: HeroSnapshot[] = agents.map((agent) => {
      const ss = systemStates[agent.key] ?? "idle";
      const st = idleStates[agent.key];
      let h = 0;
      for (const ch of agent.key) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
      return {
        sessionId: agent.key,
        title: agent.display_name,
        agent: (agent.provider || "claude") as any,
        model: agent.model || "",
        projectDir: "aman",
        projectName: "Aman",
        teamColor: h % 8,
        state: mapState(ss, agent.is_active),
        currentTool: st?.kind,
        tokens: { input: 0, output: 0 },
        startedAt: now,
        lastActivityAt: now,
      } as HeroSnapshot;
    });
    useWorld.getState().setHeroes(heroes);
  }

  // ── PixiJS lifecycle ────────────────────────────────────────────────────

  onMount(async () => {
    try {
      const { GameView } = await import("../lib/aoa/game/view");
      const fantasy = getTheme("fantasy");

      gameView = new GameView(fantasy, "en", false);
      await gameView.init(hostEl);

      // Push initial heroes AFTER GameView is ready
      pushHeroes();
      status = "ready";
    } catch (err: any) {
      status = "error";
      console.error("[AoaRealm] init failed:", err);
    }
  });

  onDestroy(() => {
    gameView?.destroy();
    gameView = null;
  });

  // React to agent data changes
  $effect(() => {
    if (agents.length > 0 && status === "ready") {
      pushHeroes();
    }
  });

  // Navigate to chat when a hero is clicked in the realm
  $effect(() => {
    if (status !== "ready") return;
    const unsub = useWorld.subscribe((state: any, prev: any) => {
      if (state.selectedSessionId && state.selectedSessionId !== prev?.selectedSessionId) {
        const agent = agents.find((a) => a.key === state.selectedSessionId);
        if (agent) onSelect(agent);
      }
    });
    return unsub;
  });
</script>

<div bind:this={hostEl} class="aoa-host">
  {#if status === "loading"}
    <div class="aoa-overlay"><p>Loading realm...</p></div>
  {:else if status === "error"}
    <div class="aoa-overlay aoa-error"><p>Realm failed to load</p></div>
  {/if}
</div>

<style>
  .aoa-host {
    width: 100%;
    height: calc(100vh - 160px);
    min-height: 400px;
    background: #1a1a17;
    border-radius: 8px;
    overflow: hidden;
    position: relative;
  }
  .aoa-overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: rgba(255,255,255,0.5);
    font-size: 14px;
    z-index: 10;
    pointer-events: none;
  }
  .aoa-error { color: rgba(248,113,113,0.7); }
  .aoa-host :global(canvas) { display: block; }
</style>
