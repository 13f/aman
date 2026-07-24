<script lang="ts">
  import { onMount, onDestroy } from "svelte";

  /**
   * ChaosRing — the "soul absorption" visual for an agent in the Chaos era.
   *
   * No halo/ring (unlike IdleRing / CognitiveRing). Instead, small colourful
   * "soul" light-points orbit the agent, spiralling inward: slow when far,
   * faster as they near the centre, where they are swallowed. The idea: the
   * agent is gathering soul-light to coalesce into an agent-man.
   *
   * Rendered only while the agenverse era is Chaos (era < 2). On Genesis the
   * parent swaps this out for the IdleRing.
   */

  let {
    emoji = "",
    imageSrc = "",
    size = 110,
    active = true,
  }: {
    emoji?: string;
    imageSrc?: string;
    size?: number;
    active?: boolean;
  } = $props();

  // ── tunables ──────────────────────────────────────────────────────────────
  const TAU = Math.PI * 2;
  const SOUL_COLORS = [
    "#FF6B9D", // pink
    "#C084FC", // purple
    "#60A5FA", // blue
    "#22D3EE", // cyan
    "#34D399", // green
    "#FBBF24", // amber
    "#FB923C", // orange
    "#F87171", // red
    "#A78BFA", // violet
  ];
  const ABSORB_DUR = 0.25; // s to fade once swallowed
  const FLASH_DUR = 0.4; // s the core glows after a swallow

  interface Soul {
    theta: number; // current angle
    r: number; // current radius (px from centre)
    spin: number; // base angular speed (rad/s)
    inward: number; // base inward speed (px/s)
    size: number; // render radius (px)
    color: string;
    opacity: number;
    twinkle: number; // phase
    absorbing: boolean; // currently being swallowed
    absorbT: number; // 0..1 progress
  }

  let canvasEl = $state<HTMLCanvasElement | null>(null);
  let ctx: CanvasRenderingContext2D | null = null;
  let particles = $state<Soul[]>([]);
  let gulp = $state(0); // core flash intensity 0..1
  let reducedMotion = $state(false);
  let rafId = 0;
  let dpr = 1;
  let cx = 0;
  let cy = 0;
  let maxR = 0;
  let absorbR = 0;

  function rand(min: number, max: number): number {
    return min + Math.random() * (max - min);
  }

  function targetCount(): number {
    // Modest population — scales with size, but never crowded.
    return Math.max(7, Math.round(size / 9));
  }

  function spawn(): Soul {
    return {
      theta: rand(0, TAU),
      r: rand(maxR * 0.4, maxR),
      spin: rand(0.4, 0.8),
      inward: rand(8, 14),
      size: rand(1.2, 2.4),
      color: SOUL_COLORS[Math.floor(Math.random() * SOUL_COLORS.length)],
      opacity: rand(0.7, 1),
      twinkle: rand(0, TAU),
      absorbing: false,
      absorbT: 0,
    };
  }

  function initParticles() {
    const n = targetCount();
    particles = Array.from({ length: n }, (_, i) => {
      const p = spawn();
      if (reducedMotion) {
        // Static ring — calm, evenly spaced, no inward motion.
        p.theta = (i / n) * TAU;
        p.r = (maxR + absorbR) / 2;
      }
      return p;
    });
  }

  function recompute() {
    cx = size / 2;
    cy = size / 2;
    maxR = size * 0.5;
    absorbR = size * 0.3;
  }

  function setupCanvas() {
    const canvas = canvasEl;
    if (!canvas) return;
    dpr = window.devicePixelRatio || 1;
    canvas.width = Math.ceil(size * dpr);
    canvas.height = Math.ceil(size * dpr);
    canvas.style.width = `${size}px`;
    canvas.style.height = `${size}px`;
    ctx = canvas.getContext("2d");
    if (ctx) ctx.scale(dpr, dpr);
  }

  function update(dt: number) {
    if (reducedMotion) {
      for (const p of particles) p.twinkle += dt; // gentle twinkle only
      return;
    }
    for (const p of particles) {
      if (p.absorbing) {
        p.absorbT += dt / ABSORB_DUR;
        if (p.absorbT >= 1) Object.assign(p, spawn());
        continue;
      }
      // Proximity multiplier: ≥1, grows as the soul nears the centre —
      // this is the "slow when far, fast when near" effect (both the orbit
      // and the inward pull accelerate together, like a vortex).
      const proximity = maxR / p.r;
      p.theta += p.spin * proximity * dt;
      p.r -= p.inward * proximity * dt;
      p.twinkle += dt;
      if (p.r <= absorbR) {
        p.absorbing = true;
        p.absorbT = 0;
        gulp = 1;
      }
    }
    if (gulp > 0) gulp = Math.max(0, gulp - dt / FLASH_DUR);
  }

  function draw() {
    if (!ctx) return;
    ctx.clearRect(0, 0, size, size);

    // Core flash — a brief glow each time a soul is swallowed.
    if (gulp > 0) {
      const g = ctx.createRadialGradient(cx, cy, 0, cx, cy, size * 0.34);
      g.addColorStop(0, `rgba(255,255,255,${0.18 * gulp})`);
      g.addColorStop(1, "rgba(255,255,255,0)");
      ctx.fillStyle = g;
      ctx.beginPath();
      ctx.arc(cx, cy, size * 0.34, 0, TAU);
      ctx.fill();
    }

    for (const p of particles) {
      const twinkle = 0.75 + 0.25 * Math.sin(p.twinkle * 3);
      const a = p.absorbing ? p.opacity * (1 - p.absorbT) : p.opacity * twinkle;
      const s = p.absorbing ? p.size * (1 - p.absorbT) : p.size;
      const x = cx + Math.cos(p.theta) * p.r;
      const y = cy + Math.sin(p.theta) * p.r;
      ctx.beginPath();
      ctx.arc(x, y, Math.max(0.1, s), 0, TAU);
      ctx.fillStyle = p.color;
      ctx.globalAlpha = a;
      ctx.shadowColor = p.color;
      ctx.shadowBlur = s * 3;
      ctx.fill();
      ctx.globalAlpha = 1;
      ctx.shadowBlur = 0;
    }
  }

  let last = 0;
  function tick(ts: number) {
    const dt = last ? Math.min(0.05, (ts - last) / 1000) : 0.016;
    last = ts;
    update(dt);
    draw();
    rafId = requestAnimationFrame(tick);
  }

  $effect(() => {
    // Reconfigure geometry + canvas whenever the requested size changes.
    recompute();
    setupCanvas();
    initParticles();
  });

  onMount(() => {
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
    reducedMotion = mq.matches;
    const onChange = (e: MediaQueryListEvent) => {
      reducedMotion = e.matches;
      initParticles();
    };
    mq.addEventListener("change", onChange);
    rafId = requestAnimationFrame(tick);
    return () => {
      mq.removeEventListener("change", onChange);
      cancelAnimationFrame(rafId);
    };
  });

  onDestroy(() => {
    cancelAnimationFrame(rafId);
  });
</script>

<div
  class="chaos-ring"
  class:dimmed={!active}
  style="width: {size}px; height: {size}px;"
  role="img"
  aria-label="Forming — absorbing soul light"
>
  <canvas bind:this={canvasEl} class="chaos-canvas" aria-hidden="true"></canvas>
  <div class="ring-center" style="font-size: {46 * (size / 110)}px;">
    {#if imageSrc}
      <img class="ring-emotion-img" src={imageSrc} alt="" />
    {:else}
      {emoji}
    {/if}
  </div>
</div>

<style>
  .chaos-ring {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    overflow: visible;
  }
  .chaos-ring.dimmed {
    opacity: 0.35;
  }
  .chaos-canvas {
    display: block;
    position: relative;
    z-index: 1;
    pointer-events: none;
  }
  .ring-center {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    user-select: none;
    cursor: default;
    line-height: 1;
    z-index: 2;
  }
  .ring-emotion-img {
    width: 65%;
    height: 65%;
    object-fit: contain;
    border-radius: 50%;
    pointer-events: none;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.4);
  }

  @media (prefers-reduced-motion: reduce) {
    .chaos-canvas {
      /* JS already freezes motion; this is a defensive no-op guard. */
      filter: none;
    }
  }
</style>
