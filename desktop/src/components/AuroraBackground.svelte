<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { createNoise2D } from "simplex-noise";

  // ── Configuration ──────────────────────────────────────────────────────

  const DOWNSAMPLE = 3;

  /**
   * Aurora layers at two poles: calm (all agents idle) and active (≥1 busy).
   * The render loop interpolates between them based on the gateway-level
   * aggregate state so the background subtly reflects overall system activity.
   */
  interface LayerDef {
    r: number; g: number; b: number;
    scale: number; speed: number; vStretch: number;
  }

  // Calm — all agents idle / sleeping: deep cool tones, slow drift.
  const CALM_LAYERS: LayerDef[] = [
    { r: 8,  g: 70,  b: 30,  scale: 1.6, speed: 0.40, vStretch: 0.18 },
    { r: 5,  g: 50,  b: 55,  scale: 2.2, speed: 0.26, vStretch: 0.14 },
    { r: 45,  g: 5,  b: 45,  scale: 2.8, speed: 0.18, vStretch: 0.25 },
    { r: 5,  g: 22,  b: 60,  scale: 4.2, speed: 0.10, vStretch: 0.32 },
  ];

  // Active — ≥1 agent working: warmer, slightly brighter, more dynamic.
  const ACTIVE_LAYERS: LayerDef[] = [
    { r: 18, g: 120, b: 30,  scale: 1.6, speed: 0.60, vStretch: 0.18 },
    { r: 10, g: 95,  b: 90,  scale: 2.2, speed: 0.42, vStretch: 0.14 },
    { r: 90,  g: 10, b: 80,  scale: 2.8, speed: 0.30, vStretch: 0.25 },
    { r: 10, g: 40,  b: 100, scale: 4.2, speed: 0.20, vStretch: 0.32 },
  ];

  // ── Aggregate gateway state (updated via SSE) ─────────────────────────

  /** Smoothed 0–1: 0 = all idle, 1 = many agents active. */
  let activity = $state(0);
  let targetActivity = $state(0);

  /** Number of agents currently reporting an error/prize state. */
  let errorCount = $state(0);

  function updateAggregate(event: any) {
    const list: Array<{ agent_id: string; system_state: string }> =
      event.payload?.agents ?? [];
    if (list.length === 0) return;

    let active = 0;
    let errors = 0;
    for (const a of list) {
      const s = a.system_state;
      if (s && s !== "idle") active++;
      if (s === "prize") errors++; // prize = agent hit an error/exception
    }
    // Map active count → 0-1 (3+ active = full)
    targetActivity = Math.min(1, active / 3);
    errorCount = errors;
  }

  // ── Canvas setup ──────────────────────────────────────────────────────

  let canvasEl = $state<HTMLCanvasElement | null>(null);
  let running = true;
  let rafId = 0;
  const noiseFns = CALM_LAYERS.map(() => createNoise2D());

  /** Per-layer accumulated phase — never jumps, only changes rate. */
  const phase: number[] = CALM_LAYERS.map(() => 0);
  let lastTime = 0;

  function lerp(a: number, b: number, t: number): number {
    return a + (b - a) * t;
  }

  function render(time: number) {
    const canvas = canvasEl;
    if (!canvas) return;

    const w = canvas.width;
    const h = canvas.height;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    // Delta time in seconds
    const dt = lastTime ? (time - lastTime) * 0.001 : 0;
    lastTime = time;

    // Smooth activity toward target
    activity = lerp(activity, targetActivity, 0.02);

    // Accumulate phase for each layer using the current interpolated speed.
    // When speed changes, only the accumulation rate changes — the absolute
    // phase never jumps, so the aurora pattern shifts smoothly.
    for (let i = 0; i < CALM_LAYERS.length; i++) {
      const calm = CALM_LAYERS[i];
      const active = ACTIVE_LAYERS[i];
      const dSpeed = lerp(calm.speed, active.speed, activity);
      phase[i] += dSpeed * dt;
    }

    const imageData = ctx.createImageData(w, h);
    const data = imageData.data;

    for (let y = 0; y < h; y++) {
      for (let x = 0; x < w; x++) {
        const nx = x / w;
        const ny = y / h;

        let r = 0, g = 0, b = 0;

        for (let i = 0; i < CALM_LAYERS.length; i++) {
          const calm = CALM_LAYERS[i];
          const active = ACTIVE_LAYERS[i];

          const dr = lerp(calm.r, active.r, activity);
          const dg = lerp(calm.g, active.g, activity);
          const db = lerp(calm.b, active.b, activity);
          const dScale = lerp(calm.scale, active.scale, activity);
          const dVStretch = lerp(calm.vStretch, active.vStretch, activity);

          // Use accumulated phase — never jumps on speed change
          const sx = nx * dScale / dVStretch + phase[i];
          const sy = ny * dScale * dVStretch + phase[i] * 0.5;
          const n = noiseFns[i](sx, sy);
          const wgt = (n + 1) * 0.5;
          r += dr * wgt;
          g += dg * wgt;
          b += db * wgt;
        }

        // Subtle red boost when agents report errors.
        if (errorCount > 0) {
          r += 8 * errorCount;
        }

        const idx = (y * w + x) * 4;
        data[idx]     = Math.min(255, r);
        data[idx + 1] = Math.min(255, g);
        data[idx + 2] = Math.min(255, b);
        data[idx + 3] = 255;
      }
    }

    ctx.putImageData(imageData, 0, 0);
  }

  function tick(now: number) {
    if (!running) return;
    render(now);
    rafId = requestAnimationFrame(tick);
  }

  let reducedMotion = $state(false);
  let mediaQuery: MediaQueryList;

  function setupCanvas(canvas: HTMLCanvasElement) {
    const dpr = window.devicePixelRatio || 1;
    const w = Math.ceil(window.innerWidth / DOWNSAMPLE);
    const h = Math.ceil(window.innerHeight / DOWNSAMPLE);
    canvas.width = w;
    canvas.height = h;
    canvas.style.width = `${window.innerWidth}px`;
    canvas.style.height = `${window.innerHeight}px`;
  }

  function onResize() {
    const canvas = canvasEl;
    if (!canvas) return;
    setupCanvas(canvas);
    // Render one frame immediately so there's no flash
    render(performance.now());
  }

  let unlistenStates: (() => void) | null = null;

  onMount(async () => {
    mediaQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
    reducedMotion = mediaQuery.matches;

    const onChange = (e: MediaQueryListEvent) => {
      reducedMotion = e.matches;
    };
    mediaQuery.addEventListener("change", onChange);

    // Subscribe to gateway aggregate state
    unlistenStates = await listen("agent_states:updated", updateAggregate);

    window.addEventListener("resize", onResize);
    if (canvasEl) {
      setupCanvas(canvasEl);
    }

    if (!reducedMotion) {
      rafId = requestAnimationFrame(tick);
    } else {
      render(0);
    }

    return () => {
      window.removeEventListener("resize", onResize);
      mediaQuery.removeEventListener("change", onChange);
    };
  });

  onDestroy(() => {
    running = false;
    if (rafId) cancelAnimationFrame(rafId);
    if (unlistenStates) unlistenStates();
  });
</script>

<canvas
  bind:this={canvasEl}
  class="aurora-canvas"
  aria-hidden="true"
></canvas>

<style>
  .aurora-canvas {
    position: fixed;
    inset: 0;
    /* Render behind the glass layers (sidebar, main) so the aurora sits
       underneath the frosted glass.  The glass layers' backdrop-filter blur
       softens the noise into natural-looking aurora glow. */
    z-index: 0;
    image-rendering: auto;
    pointer-events: none;
  }
</style>
