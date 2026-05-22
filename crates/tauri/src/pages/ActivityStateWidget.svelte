<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";
  import IdleRing from "./IdleRing.svelte";

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

  interface MetricsData {
    queue_depth: { high: number; normal: number; low: number };
    inflight_pipelines: number;
    inflight_skills: number;
    backpressure_level: string;
  }

  type Mode = "idle" | "reflection" | "processing";

  // ---------------------------------------------------------------------------
  // Props
  // ---------------------------------------------------------------------------

  let {
    visible = true,
    agentId = "",
    agentName = "",
    runtimeRunning = false,
  }: {
    visible?: boolean;
    agentId?: string;
    agentName?: string;
    runtimeRunning?: boolean;
  } = $props();

  // ---------------------------------------------------------------------------
  // State
  // ---------------------------------------------------------------------------

  let mode = $state<Mode>("idle");
  let idleSnap = $state<IdleSnap | null>(null);
  let reflectSnap = $state<ReflectSnap | null>(null);
  let metrics = $state<MetricsData | null>(null);
  let unlisteners: (() => void)[] = [];

  $effect(() => {
    if (!runtimeRunning) {
      mode = "idle";
      idleSnap = null;
      reflectSnap = null;
      metrics = null;
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

  const COLORS: Record<Mode, { outer: string; inner: string }> = {
    idle:       { outer: "#6c8cff", inner: "#f59e0b" },
    reflection: { outer: "#a78bfa", inner: "#f472b6" },
    processing: { outer: "#4ade80", inner: "#22d3ee" },
  };

  const MODE_ICON: Record<Mode, string> = {
    idle:       "\u{1F4A4}",
    reflection: "\u{1F9E0}",
    processing: "\u{26A1}",
  };

  // --- derived ---

  let totalQueue = $derived(
    (metrics?.queue_depth.high ?? 0) +
    (metrics?.queue_depth.normal ?? 0) +
    (metrics?.queue_depth.low ?? 0)
  );
  let totalInflight = $derived(
    (metrics?.inflight_pipelines ?? 0) + (metrics?.inflight_skills ?? 0)
  );

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
    if (mode === "idle" && idleSnap) return depthPct(idleSnap.depth);
    if (mode === "reflection" && reflectSnap) return reflectSnap.arousalLevel * 100;
    if (mode === "processing") return Math.min(100, totalQueue * 5);
    return 0;
  });

  let innerPct = $derived.by(() => {
    if (mode === "idle" && idleSnap) return Math.round(idleSnap.arousal * 100);
    if (mode === "reflection" && reflectSnap)
      return Math.min(100, reflectSnap.reflectionConsecutiveCount * 10);
    if (mode === "processing") return Math.min(100, totalInflight * 20);
    return 0;
  });

  let emoji = $derived.by(() => {
    if (mode === "idle") return idleSnap ? (IDLE_EMOJI[idleSnap.kind] ?? MODE_ICON.idle) : MODE_ICON.idle;
    return MODE_ICON[mode];
  });

  let label = $derived.by(() => {
    if (mode === "idle") return "IDLE" + (idleSnap ? "/" + idleSnap.kind : "");
    if (mode === "reflection") return "REFLECTION";
    return "PROCESSING";
  });

  let info1 = $derived.by(() => {
    if (mode === "idle") return `Depth: ${Math.round(outerPct)}%`;
    if (mode === "reflection") return `Arousal: ${Math.round(outerPct)}%`;
    return `Queue: ${totalQueue}`;
  });

  let info2 = $derived.by(() => {
    if (mode === "idle") return `Arousal: ${innerPct}%`;
    if (mode === "reflection")
      return `Cycle: ${reflectSnap?.reflectionConsecutiveCount ?? 0}`;
    if (mode === "processing") {
      const bp = metrics?.backpressure_level;
      return bp && bp !== "Normal" ? `BP: ${bp}` : `Inflight: ${totalInflight}`;
    }
    return "";
  });

  let ringColors = $derived(COLORS[mode]);

  // ---------------------------------------------------------------------------
  // Event handlers
  // ---------------------------------------------------------------------------

  function matchesAgent(data: any): boolean {
    if (!agentId) return true;
    const eventAgentId: string | undefined = data.agent_id ?? data.payload?.agent_id;
    if (!eventAgentId) return true; // global events pass through
    return eventAgentId === agentId;
  }

  function onEvent(e: any) {
    const p = e.payload;
    if (!p?.event_type) return;

    const et: string = p.event_type;
    const data = p.payload ?? {};

    // Filter by agent when an agentId is specified
    if (!matchesAgent(data)) return;

    if (et === "idle") {
      const d = data;
      idleSnap = {
        kind: d.kind ?? "daze",
        depth: d.depth ?? 0,
        arousal: d.context?.arousal_level ?? 0.5,
      };
      mode = "idle";
    } else if (et === "system.queue_drained") {
      const d = data;
      reflectSnap = {
        lastEventType: d.lastEventType ?? "",
        arousalLevel: d.arousalLevel ?? 0.5,
        reflectionConsecutiveCount: d.reflectionConsecutiveCount ?? 0,
      };
      mode = "reflection";
    } else {
      mode = "processing";
    }
  }

  function onMetrics(e: any) {
    const m = e.payload;
    metrics = m;
    if (m && (m.inflight_pipelines > 0 || m.inflight_skills > 0) && mode === "idle") {
      mode = "processing";
    }
  }

  onMount(async () => {
    unlisteners.push(await listen("event:processed", onEvent));
    unlisteners.push(await listen("metrics:updated", onMetrics));
  });

  onDestroy(() => {
    for (const fn of unlisteners) fn();
  });
</script>

{#if visible}
  <div class="activity-widget" class:active={runtimeRunning}>
    {#if agentName}
      <div class="agent-label">{agentName}</div>
    {/if}

    <IdleRing
      {mode}
      {outerPct}
      {innerPct}
      {emoji}
      {label}
      {info1}
      {info2}
      {ringColors}
      size={110}
      active={runtimeRunning}
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
