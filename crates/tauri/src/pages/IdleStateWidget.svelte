<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { onMount, onDestroy } from "svelte";

  interface IdleSnapshot {
    kind: string;
    depth: number;
    arousal: number;
  }

  let { runtimeRunning = false }: { runtimeRunning?: boolean } = $props();

  let snapshot = $state<IdleSnapshot | null>(null);
  let unlisteners: (() => void)[] = [];

  const THRESHOLDS = [0, 5, 20, 50, 100, 200];

  const EMOJI: Record<string, string> = {
    Daze: "😶",
    Boredom: "😒",
    Sleep: "😴",
    Exploration: "🔍",
    Meditation: "🧘",
    Incubation: "💡",
    Waiting: "⏳",
  };

  const LABELS: Record<string, string> = {
    Daze: "Daze — 茫然",
    Boredom: "Boredom — 无聊",
    Sleep: "Sleep — 休眠",
    Exploration: "Exploration — 探索",
    Meditation: "Meditation — 冥想",
    Incubation: "Incubation — 孵化",
    Waiting: "Waiting — 等待",
  };

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

  let depthProgress = $derived(snapshot ? depthPct(snapshot.depth) : 0);
  let arousalProgress = $derived(snapshot ? Math.round(snapshot.arousal * 100) : 0);
  let emoji = $derived(snapshot ? (EMOJI[snapshot.kind] ?? "💤") : "💤");
  let kindName = $derived(snapshot ? (LABELS[snapshot.kind] ?? snapshot.kind) : "Idle");
  let active = $derived(runtimeRunning && snapshot !== null);

  // Outer circle: depth | Inner circle: arousal
  const OUTER_R = 48;
  const INNER_R = 34;
  const OUTER_CIRCUM = 2 * Math.PI * OUTER_R; // ~301.6
  const INNER_CIRCUM = 2 * Math.PI * INNER_R; // ~213.6

  function dashOffset(circum: number, pct: number): number {
    return circum - (pct / 100) * circum;
  }

  function handleEvent(e: any) {
    const p = e.payload;
    if (p?.event_type === "idle") {
      const data = p.payload ?? {};
      snapshot = {
        kind: data.kind ?? "Daze",
        depth: data.depth ?? 0,
        arousal: data.context?.arousal_level ?? 0.5,
      };
    }
  }

  onMount(async () => {
    unlisteners.push(await listen("event:processed", handleEvent));
  });

  onDestroy(() => {
    for (const fn of unlisteners) fn();
  });
</script>

<div class="idle-widget" class:active>
  <div class="idle-label">IDLE</div>
  <div class="ring-wrapper">
    <svg viewBox="0 0 110 110" class="ring-svg">
      <!-- track rings -->
      <circle cx="55" cy="55" r={OUTER_R} fill="none" stroke="#2a2d3a" stroke-width="5" />
      <circle cx="55" cy="55" r={INNER_R} fill="none" stroke="#2a2d3a" stroke-width="5" />
      <!-- depth ring (outer) -->
      <circle cx="55" cy="55" r={OUTER_R} fill="none" stroke="#6c8cff" stroke-width="5"
        stroke-dasharray={OUTER_CIRCUM} stroke-dashoffset={dashOffset(OUTER_CIRCUM, depthProgress)}
        stroke-linecap="round" transform="rotate(-90 55 55)" />
      <!-- arousal ring (inner) -->
      <circle cx="55" cy="55" r={INNER_R} fill="none" stroke="#f59e0b" stroke-width="5"
        stroke-dasharray={INNER_CIRCUM} stroke-dashoffset={dashOffset(INNER_CIRCUM, arousalProgress)}
        stroke-linecap="round" transform="rotate(-90 55 55)" />
    </svg>
    <div class="ring-center" title={kindName}>{emoji}</div>
  </div>
  <div class="ring-labels">
    <span class="pct depth">{Math.round(depthProgress)}%</span>
    <span class="pct arousal">{arousalProgress}%</span>
  </div>
</div>

<style>
  .idle-widget {
    margin-top: auto;
    padding: 14px 16px 20px;
    border-top: 1px solid var(--border);
    text-align: center;
    opacity: 0.35;
    transition: opacity 0.4s;
  }
  .idle-widget.active {
    opacity: 1;
  }
  .idle-label {
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
    pointer-events: none;
    user-select: none;
  }
  .ring-labels {
    display: flex;
    justify-content: center;
    gap: 14px;
    margin-top: 6px;
  }
  .pct {
    font-size: 10px;
    font-weight: 700;
  }
  .pct.depth {
    color: #6c8cff;
  }
  .pct.arousal {
    color: #f59e0b;
  }
</style>
