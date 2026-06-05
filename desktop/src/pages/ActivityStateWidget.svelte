<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";
  import IdleRing from "./IdleRing.svelte";
  import { loadEmotions, resolveEmotionImage } from "../lib/emotions";
  import type { EmotionsConfig } from "../lib/emotions";

  // ---------------------------------------------------------------------------
  // Types
  // ---------------------------------------------------------------------------

  interface IdleSnap {
    kind: string;
    depth: number;
    arousal: number;
  }

  interface ReflectSnap {
    lastEventType: string;
    arousalLevel: number;
    reflectionConsecutiveCount: number;
  }

  // idleSubMode only matters when the agent is idle; it distinguishes
  // quiet-idle from the reflection phase that follows queue drain.
  type IdleSubMode = "idle" | "reflection";

  // ---------------------------------------------------------------------------
  // Props
  // ---------------------------------------------------------------------------

  let {
    visible = true,
    agentId = "",
    agentName = "",
    runtimeRunning = false,
    compact = false,
  }: {
    visible?: boolean;
    agentId?: string;
    agentName?: string;
    runtimeRunning?: boolean;
    compact?: boolean;
  } = $props();

  // ---------------------------------------------------------------------------
  // State
  // ---------------------------------------------------------------------------

  let idleSubMode = $state<IdleSubMode>("idle");
  let idleSnap = $state<IdleSnap | null>(null);
  let reflectSnap = $state<ReflectSnap | null>(null);
  let systemState = $state<string>("");
  let llmEmotionId = $state<string>("");
  let emotionsConfig = $state<EmotionsConfig | null>(null);
  let unlisteners: (() => void)[] = [];

  $effect(() => {
    if (!runtimeRunning) {
      idleSubMode = "idle";
      idleSnap = null;
      reflectSnap = null;
      systemState = "";
      llmEmotionId = "";
    }
  });

  // Reload emotions config whenever the selected agent changes.
  $effect(() => {
    if (agentId) {
      loadEmotions(agentId).then((cfg) => {
        emotionsConfig = cfg;
      });
    } else {
      emotionsConfig = null;
    }
  });

  // --- constants ---

  const THRESHOLDS = [0, 5, 20, 50, 100, 200];

  const IDLE_EMOJI: Record<string, string> = {
    daze: "\u{1F636}", boredom: "\u{1F612}", sleep: "\u{1F634}",
    exploration: "\u{1F50D}", meditation: "\u{1F9D8}",
    incubation: "\u{1F4A1}", waiting: "\u{23F3}",
  };

  const IDLE_LABEL: Record<string, string> = {
    daze: "Daze", boredom: "Boredom",
    sleep: "Sleep", exploration: "Exploration",
    meditation: "Meditation",
    incubation: "Incubation", waiting: "Waiting",
  };

  const SS_LABEL: Record<string, string> = {
    idle: "Idle", working: "Working", chatting: "Chatting",
    studying: "Studying", daily_life: "Daily Life",
    waiting: "Waiting",
  };

  const STATE_EMOJI: Record<string, string> = {
    chatting: "\u{1F4AC}",   // 💬
    working: "\u{1F6E0}\u{FE0F}",  // 🛠️
    studying: "\u{1F4DA}",   // 📚
    daily_life: "\u{1F3E0}", // 🏠
    waiting: "\u{23F3}",     // ⏳
  };

  const COLORS: Record<string, { outer: string; inner: string }> = {
    idle:       { outer: "#6c8cff", inner: "#f59e0b" },
    reflection: { outer: "#a78bfa", inner: "#f472b6" },
    chatting:   { outer: "#4ade80", inner: "#22d3ee" },
    working:    { outer: "#f59e0b", inner: "#fbbf24" },
    studying:   { outer: "#a78bfa", inner: "#c4b5fd" },
    daily_life: { outer: "#fb923c", inner: "#fdba74" },
    waiting:    { outer: "#f59e0b", inner: "#fbbf24" },
  };

  // --- derived: is the agent in an active lifecycle state? ---

  let isActive = $derived(
    systemState !== "idle" && systemState !== "" && systemState in SS_LABEL
  );

  let displayState = $derived(
    isActive ? systemState : idleSubMode
  );

  // --- ring values ---

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

  let outerPct = $derived.by(() => {
    if (displayState === "idle" && idleSnap) return depthPct(idleSnap.depth);
    if (displayState === "reflection" && reflectSnap) return reflectSnap.arousalLevel * 100;
    return 0;
  });

  let innerPct = $derived.by(() => {
    if (displayState === "idle" && idleSnap) return Math.round(idleSnap.arousal * 100);
    if (displayState === "reflection" && reflectSnap)
      return Math.min(100, reflectSnap.reflectionConsecutiveCount * 10);
    return 0;
  });

  // --- display ---

  let emoji = $derived.by(() => {
    if (displayState === "idle")
      return idleSnap ? (IDLE_EMOJI[idleSnap.kind] ?? "\u{1F4A4}") : "\u{1F4A4}";
    if (displayState === "reflection") return "\u{1F9E0}";
    return STATE_EMOJI[displayState] ?? "\u{26A1}";
  });

  let label = $derived.by(() => {
    if (displayState === "idle")
      return "Idle" + (idleSnap ? "/" + IDLE_LABEL[idleSnap.kind] : "");
    if (displayState === "reflection") return "Idle/Reflection";
    return SS_LABEL[displayState] ?? displayState;
  });

  let info1 = $derived.by(() => {
    if (displayState === "idle") return `Depth: ${Math.round(outerPct)}%`;
    if (displayState === "reflection") return `Arousal: ${Math.round(outerPct)}%`;
    return "";
  });

  let info2 = $derived.by(() => {
    if (displayState === "idle") return `Arousal: ${innerPct}%`;
    if (displayState === "reflection")
      return `Cycle: ${reflectSnap?.reflectionConsecutiveCount ?? 0}`;
    return "";
  });

  let ringColors = $derived(COLORS[displayState] ?? COLORS["idle"]);

  // Resolve the emotion image for the current display state.
  // Priority: LLM emotion (from gateway) > state-based mapping.
  let emotionKind = $derived.by(() => {
    if (llmEmotionId) return llmEmotionId;
    if (displayState === "idle") return idleSnap?.kind ?? "idle";
    if (displayState === "reflection") return "reflection";
    return displayState; // active system state key
  });

  let emotionImage = $derived(
    resolveEmotionImage(emotionsConfig, emotionKind) ?? "",
  );

  // ---------------------------------------------------------------------------
  // Event handlers
  // ---------------------------------------------------------------------------

  function matchesAgent(data: any): boolean {
    if (!agentId) return true;
    const eventAgentId: string | undefined = data.agent_id ?? data.payload?.agent_id;
    if (!eventAgentId) return true;
    return eventAgentId === agentId;
  }

  function onEvent(e: any) {
    // Only track idle/reflection events when the agent is actually idle.
    if (systemState !== "idle" && systemState !== "") return;

    const p = e.payload;
    if (!p?.event_type) return;
    const et: string = p.event_type;
    const data = p.payload ?? {};
    if (!matchesAgent(data)) return;

    if (et === "idle") {
      idleSnap = {
        kind: data.kind ?? "daze",
        depth: data.depth ?? 0,
        arousal: data.context?.arousal_level ?? 0.5,
      };
      idleSubMode = "idle";
    } else if (et === "system.queue_drained") {
      reflectSnap = {
        lastEventType: data.lastEventType ?? "",
        arousalLevel: data.arousalLevel ?? 0.5,
        reflectionConsecutiveCount: data.reflectionConsecutiveCount ?? 0,
      };
      idleSubMode = "reflection";
    }
  }

  onMount(async () => {
    unlisteners.push(await listen("event:processed", onEvent));
    unlisteners.push(await listen("agent_states:updated", (e: any) => {
      const list: Array<{ agent_id: string; system_state: string; emotion_id?: string }> = e.payload?.agents ?? [];
      for (const a of list) {
        if (!agentId || a.agent_id === agentId) {
          systemState = a.system_state;
          if (a.emotion_id) {
            llmEmotionId = a.emotion_id;
          }
          break;
        }
      }
    }));
  });

  onDestroy(() => {
    for (const fn of unlisteners) fn();
  });
</script>

{#if visible}
  <div class="activity-widget" class:active={runtimeRunning} class:compact>
    {#if agentName && !compact}
      <div class="agent-label">{agentName}</div>
    {/if}

    <IdleRing
      mode={displayState}
      {outerPct}
      {innerPct}
      {emoji}
      imageSrc={emotionImage}
      {label}
      {info1}
      {info2}
      {ringColors}
      size={compact ? 36 : 110}
      active={runtimeRunning}
      showLabel={!compact}
      showInfo={!compact && !isActive}
    />

  </div>
{/if}

<style>
  .activity-widget {
    margin-top: auto;
    padding: 14px 16px 20px;
    border-top: 1px solid var(--border);
    text-align: center;
    display: flex;
    flex-direction: column;
    align-items: center;
  }
  .activity-widget.compact {
    padding: 8px 4px;
  }
  .agent-label {
    font-size: 11px;
    font-weight: 600;
    color: var(--accent);
    margin-bottom: 8px;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
