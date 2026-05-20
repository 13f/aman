<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";

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

  let { runtimeRunning = false }: { runtimeRunning?: boolean } = $props();

  // ---------------------------------------------------------------------------
  // State
  // ---------------------------------------------------------------------------

  let mode = $state<Mode>("idle");
  let idleSnap = $state<IdleSnap | null>(null);
  let reflectSnap = $state<ReflectSnap | null>(null);
  let metrics = $state<MetricsData | null>(null);
  let unlisteners: (() => void)[] = [];

  // Reset all state when the gateway stops so stale idle/metrics data
  // doesn't linger after a stop/restart cycle.
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
    daze: "Daze — \u{76F2}\u{7136}", boredom: "Boredom — \u{65E0}\u{804A}",
    sleep: "Sleep — \u{4F11}\u{7720}", exploration: "Exploration — \u{63A2}\u{7D22}",
    meditation: "Meditation — \u{51A5}\u{60F3}",
    incubation: "Incubation — \u{5B75}\u{5316}", waiting: "Waiting — \u{7B49}\u{5F85}",
  };

  // Mode-specific ring colors
  const COLORS: Record<Mode, { outer: string; inner: string }> = {
    idle:       { outer: "#6c8cff", inner: "#f59e0b" },
    reflection: { outer: "#a78bfa", inner: "#f472b6" },
    processing: { outer: "#4ade80", inner: "#22d3ee" },
  };

  // Center emoji per mode
  const MODE_ICON: Record<Mode, string> = {
    idle:       "\u{1F4A4}",   // 💤 fallback when no snapshot
    reflection: "\u{1F9E0}",   // 🧠
    processing: "\u{26A1}",    // ⚡
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

  let title = $derived.by(() => {
    if (mode === "idle" && idleSnap) return IDLE_LABEL[idleSnap.kind] ?? idleSnap.kind;
    if (mode === "reflection")
      return "Reflecting after " + (reflectSnap?.lastEventType ?? "?");
    return "Processing " + totalQueue + " queued events";
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
      return bp && bp !== "Normal" ? `Backpressure: ${bp}` : `Inflight: ${totalInflight}`;
    }
    return "";
  });

  let ringColors = $derived(COLORS[mode]);

  // --- SVG ring geometry ---

  const R_OUTER = 48;
  const R_INNER = 34;
  const C_OUTER = 2 * Math.PI * R_OUTER;
  const C_INNER = 2 * Math.PI * R_INNER;

  function dash(circum: number, pct: number): number {
    return circum - (pct / 100) * circum;
  }

  // ---------------------------------------------------------------------------
  // Event handlers
  // ---------------------------------------------------------------------------

  function onEvent(e: any) {
    const p = e.payload;
    if (!p?.event_type) return;

    const et: string = p.event_type;

    if (et === "idle") {
      const d = p.payload ?? {};
      idleSnap = {
        kind: d.kind ?? "daze",
        depth: d.depth ?? 0,
        arousal: d.context?.arousal_level ?? 0.5,
      };
      mode = "idle";
    } else if (et === "system.queue_drained") {
      const d = p.payload ?? {};
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

<div class="activity-widget" class:active={runtimeRunning}>
  <div class="label-text">{label}</div>

  <div class="ring-wrapper">
    <svg viewBox="0 0 110 110" class="ring-svg">
      <!-- track rings -->
      <circle cx="55" cy="55" r={R_OUTER} fill="none" stroke="#2a2d3a" stroke-width="5" />
      <circle cx="55" cy="55" r={R_INNER} fill="none" stroke="#2a2d3a" stroke-width="5" />

      <!-- outer ring (progress) -->
      <circle cx="55" cy="55" r={R_OUTER} fill="none" stroke={ringColors.outer} stroke-width="5"
        stroke-dasharray={C_OUTER} stroke-dashoffset={dash(C_OUTER, outerPct)}
        stroke-linecap="round" transform="rotate(-90 55 55)" />

      <!-- inner ring (secondary) -->
      <circle cx="55" cy="55" r={R_INNER} fill="none" stroke={ringColors.inner} stroke-width="5"
        stroke-dasharray={C_INNER} stroke-dashoffset={dash(C_INNER, innerPct)}
        stroke-linecap="round" transform="rotate(-90 55 55)" />
    </svg>

    <div class="ring-center" title={title}>{emoji}</div>
  </div>

  <div class="info-lines">
    <span class="info-outer" style="color: {ringColors.outer}">{info1}</span>
    <span class="info-inner" style="color: {ringColors.inner}">{info2}</span>
  </div>
</div>

<style>
  .activity-widget {
    margin-top: auto;
    padding: 14px 16px 20px;
    border-top: 1px solid var(--border);
    text-align: center;
    opacity: 0.35;
    transition: opacity 0.4s;
  }
  .activity-widget.active {
    opacity: 1;
  }
  .label-text {
    font-size: 10px;
    color: var(--fg-dim);
    letter-spacing: 1.5px;
    margin-bottom: 10px;
  }
  .ring-wrapper {
    position: relative;
    width: 110px;
    height: 110px;
    margin: 0 auto;
  }
  .ring-svg {
    width: 100%;
    height: 100%;
  }
  .ring-center {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 30px;
    user-select: none;
    cursor: default;
  }
  .info-lines {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    margin-top: 6px;
  }
  .info-outer, .info-inner {
    font-size: 10px;
    font-weight: 700;
    line-height: 1.5;
  }
</style>
