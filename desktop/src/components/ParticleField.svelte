<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";

  // ── Configuration ──────────────────────────────────────────────────────

  /** Base particle count when all agents are idle. */
  const COUNT_IDLE = 30;
  /** Max particle count when ≥3 agents are active. */
  const COUNT_ACTIVE = 50;
  /** Speed multiplier at full activity (idle speed × this = active speed). */
  const SPEED_ACTIVE_MULT = 1.8;
  /** Opacity range — kept very low so particles are barely noticed. */
  const OPACITY_MIN = 0.08;
  const OPACITY_MAX = 0.22;
  /** Particle radius range in CSS pixels. */
  const SIZE_MIN = 1.0;
  const SIZE_MAX = 3.0;
  /** How strongly particles are pulled toward a target (0–1). */
  const GRAVITY_STRENGTH = 0.015;
  /** Damping applied each frame (1 = no damping). */
  const DAMPING = 0.998;
  /** Max drift speed in px/frame. */
  const MAX_SPEED_IDLE = 0.35;
  const MAX_SPEED_ACTIVE = 0.65;

  // ── Particle type ──────────────────────────────────────────────────────

  interface Particle {
    x: number;
    y: number;
    vx: number;
    vy: number;
    size: number;
    opacity: number;
    /** Gravitational target — particles drift toward this point then scatter. */
    targetX: number | null;
    targetY: number | null;
  }

  // ── Reactive state ─────────────────────────────────────────────────────

  /** 0–1 smoothed activity level (0 = all idle, 1 = busy). */
  let activity = $state(0);
  let targetActivity = $state(0);

  /** Optional attractor point for "new message" gravitational effect. */
  let attractorX = $state<number | null>(null);
  let attractorY = $state<number | null>(null);
  let attractorTimer = 0;

  let canvasEl = $state<HTMLCanvasElement | null>(null);
  let running = true;
  let rafId = 0;
  let particles: Particle[] = [];
  let w = 0;
  let h = 0;
  let reducedMotion = $state(false);

  // ── SSE: gateway aggregate agent state ─────────────────────────────────

  function updateAggregate(event: any) {
    const list: Array<{
      agent_id: string;
      system_state: string;
      cognitive_state?: string;
    }> = event.payload?.agents ?? [];
    if (list.length === 0) return;

    let active = 0;
    // Cognitive-state-weighted activity: degraded agents don't count as "active".
    for (const a of list) {
      const cog = a.cognitive_state ?? "Lucid";
      if (a.system_state && a.system_state !== "idle" && cog === "Lucid") active++;
    }
    targetActivity = Math.min(1, active / 3);
  }

  // ── Particle management ────────────────────────────────────────────────

  function desiredCount(): number {
    return Math.round(COUNT_IDLE + (COUNT_ACTIVE - COUNT_IDLE) * activity);
  }

  function spawnParticle(): Particle {
    return {
      x: Math.random() * w,
      y: Math.random() * h,
      vx: (Math.random() - 0.5) * 0.2,
      vy: (Math.random() - 0.5) * 0.2,
      size: SIZE_MIN + Math.random() * (SIZE_MAX - SIZE_MIN),
      opacity: OPACITY_MIN + Math.random() * (OPACITY_MAX - OPACITY_MIN),
      targetX: null,
      targetY: null,
    };
  }

  function adjustParticleCount() {
    const target = desiredCount();
    while (particles.length < target) {
      particles.push(spawnParticle());
    }
    while (particles.length > target) {
      // Fade out before removing so it doesn't pop
      particles.pop();
    }
  }

  // ── Public API: trigger attractor ──────────────────────────────────────

  /**
   * Briefly attract particles toward a point on screen (e.g. when a new
   * message arrives). Particles drift toward the point, then scatter.
   * Called from outside via the component reference.
   */
  export function attractTo(x: number, y: number) {
    attractorX = x;
    attractorY = y;
    attractorTimer = 120; // ~2 seconds at 60fps

    // Assign the target to a random subset of particles
    const count = Math.floor(particles.length * 0.4);
    for (let i = 0; i < count; i++) {
      const p = particles[Math.floor(Math.random() * particles.length)];
      p.targetX = x + (Math.random() - 0.5) * 80;
      p.targetY = y + (Math.random() - 0.5) * 40;
    }
  }

  // ── Render loop ────────────────────────────────────────────────────────

  function render(_time: number) {
    const canvas = canvasEl;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    // Smooth activity toward target
    activity += (targetActivity - activity) * 0.03;
    adjustParticleCount();

    // Decay attractor
    if (attractorTimer > 0) {
      attractorTimer--;
      if (attractorTimer === 0) {
        attractorX = null;
        attractorY = null;
      }
    }

    const maxSpeed = MAX_SPEED_IDLE + (MAX_SPEED_ACTIVE - MAX_SPEED_IDLE) * activity;

    ctx.clearRect(0, 0, w, h);

    for (const p of particles) {
      // ── Gravitational pull toward target ──
      if (p.targetX !== null && p.targetY !== null) {
        const dx = p.targetX - p.x;
        const dy = p.targetY - p.y;
        const dist = Math.sqrt(dx * dx + dy * dy);
        if (dist < 3) {
          // Reached target — scatter in a random direction
          p.targetX = null;
          p.targetY = null;
          const angle = Math.random() * Math.PI * 2;
          p.vx += Math.cos(angle) * 0.3;
          p.vy += Math.sin(angle) * 0.3;
        } else {
          p.vx += (dx / dist) * GRAVITY_STRENGTH;
          p.vy += (dy / dist) * GRAVITY_STRENGTH;
        }
      }

      // ── Subtle random drift ──
      p.vx += (Math.random() - 0.5) * 0.008;
      p.vy += (Math.random() - 0.5) * 0.008;

      // ── Damping ──
      p.vx *= DAMPING;
      p.vy *= DAMPING;

      // ── Speed clamp ──
      const speed = Math.sqrt(p.vx * p.vx + p.vy * p.vy);
      if (speed > maxSpeed) {
        p.vx = (p.vx / speed) * maxSpeed;
        p.vy = (p.vy / speed) * maxSpeed;
      }

      // ── Move ──
      p.x += p.vx;
      p.y += p.vy;

      // ── Wrap edges with padding ──
      const pad = 20;
      if (p.x < -pad) p.x = w + pad;
      if (p.x > w + pad) p.x = -pad;
      if (p.y < -pad) p.y = h + pad;
      if (p.y > h + pad) p.y = -pad;

      // ── Draw soft glowing dot ──
      // Warmer tint when activity is high, cool blue-white when idle.
      const warmR = 200 + activity * 55;
      const warmG = 210 + activity * 30;
      const warmB = 240 - activity * 40;
      ctx.beginPath();
      ctx.arc(p.x, p.y, p.size, 0, Math.PI * 2);
      ctx.fillStyle = `rgba(${Math.round(warmR)}, ${Math.round(warmG)}, ${Math.round(warmB)}, ${p.opacity})`;
      ctx.shadowColor = `rgba(${Math.round(warmR)}, ${Math.round(warmG)}, ${Math.round(warmB)}, ${p.opacity * 0.6})`;
      ctx.shadowBlur = p.size * 2.5;
      ctx.fill();
      // Reset shadow for next draw
      ctx.shadowBlur = 0;
    }
  }

  function tick(now: number) {
    if (!running) return;
    render(now);
    rafId = requestAnimationFrame(tick);
  }

  // ── Canvas setup ───────────────────────────────────────────────────────

  function setupCanvas(canvas: HTMLCanvasElement) {
    const dpr = window.devicePixelRatio || 1;
    w = window.innerWidth;
    h = window.innerHeight;
    canvas.width = Math.ceil(w * dpr);
    canvas.height = Math.ceil(h * dpr);
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;
    const ctx = canvas.getContext("2d");
    if (ctx) ctx.scale(dpr, dpr);
  }

  function onResize() {
    const canvas = canvasEl;
    if (!canvas) return;
    setupCanvas(canvas);
    // Reinitialize particles for new dimensions
    particles = [];
    adjustParticleCount();
  }

  let unlistenStates: (() => void) | null = null;
  let mediaQuery: MediaQueryList;

  onMount(async () => {
    mediaQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
    reducedMotion = mediaQuery.matches;

    const onChange = (e: MediaQueryListEvent) => {
      reducedMotion = e.matches;
    };
    mediaQuery.addEventListener("change", onChange);

    unlistenStates = await listen("agent_states:updated", updateAggregate);

    window.addEventListener("resize", onResize);
    if (canvasEl) {
      setupCanvas(canvasEl);
    }

    // Seed initial particles
    particles = Array.from({ length: COUNT_IDLE }, () => spawnParticle());

    if (!reducedMotion) {
      rafId = requestAnimationFrame(tick);
    } else {
      // Still render one static frame
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
  class="particle-canvas"
  aria-hidden="true"
></canvas>

<style>
  .particle-canvas {
    position: fixed;
    inset: 0;
    /* Sit between aurora (z-index: 0) and glass UI (z-index: auto / 2+). */
    z-index: 1;
    pointer-events: none;
  }
</style>
