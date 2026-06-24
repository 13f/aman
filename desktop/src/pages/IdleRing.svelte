<script lang="ts">
  type Mode = "idle" | "reflection" | "processing";

  // Stable unique id per component instance for SVG gradient references.
  let _uid = "";
  function uid(): string {
    if (!_uid) _uid = "ir" + Math.random().toString(36).slice(2, 8);
    return _uid;
  }

  let {
    mode = "idle" as Mode,
    outerPct = 0,
    innerPct = 0,
    emoji = "",
    imageSrc = "",
    label = "",
    info1 = "",
    info2 = "",
    ringColors = { outer: "#6c8cff", inner: "#f59e0b" },
    size = 110,
    active = true,
    showLabel = true,
    showInfo = true,
    /** One-shot effect trigger: "pulse" | "shake" | "wakeup". Plays once, then
     *  auto-resets. Set to a different value to re-trigger the same effect. */
    trigger = null as string | null,
  }: {
    mode?: Mode;
    outerPct?: number;
    innerPct?: number;
    emoji?: string;
    /** Optional image data URL — when set, shown instead of the emoji glyph. */
    imageSrc?: string;
    label?: string;
    info1?: string;
    info2?: string;
    ringColors?: { outer: string; inner: string };
    size?: number;
    active?: boolean;
    showLabel?: boolean;
    showInfo?: boolean;
    trigger?: string | null;
  } = $props();

  const R_OUTER = 48;
  const R_INNER = 41;
  const C_OUTER = 2 * Math.PI * R_OUTER;
  const C_INNER = 2 * Math.PI * R_INNER;

  function dash(circum: number, pct: number): number {
    return circum - (Math.min(100, Math.max(0, pct)) / 100) * circum;
  }

  let outerDash = $derived(dash(C_OUTER, outerPct));
  let innerDash = $derived(dash(C_INNER, innerPct));
  let scale = $derived(size / 110);

  // Glow colour derived from the outer ring colour.
  let glowColor = $derived(ringColors.outer);

  // ── Effect system ──────────────────────────────────────────────────────

  let activeEffect = $state<string | null>(null);
  let effectTimer = $state<ReturnType<typeof setTimeout> | null>(null);

  // Duration of each one-shot effect in ms (must match CSS animation-duration).
  const EFFECT_DURATION: Record<string, number> = {
    pulse: 500,
    shake: 350,
    wakeup: 700,
  };

  $effect(() => {
    const e = trigger;
    if (e && e !== activeEffect) {
      if (effectTimer) clearTimeout(effectTimer);
      activeEffect = e;
      const ms = EFFECT_DURATION[e] ?? 500;
      effectTimer = setTimeout(() => {
        activeEffect = null;
        effectTimer = null;
      }, ms);
    }
  });

  // Continuous effect from mode (only when no one-shot is playing + runtime active).
  let effectClass = $derived(
    !active
      ? null
      : activeEffect
        ? activeEffect
        : mode === "idle" || mode === "wakeup"
          ? "breathing"
          : mode === "reflection" || mode === "processing"
            ? "ripple"
            : null,
  );
</script>

<div
  class="idle-ring"
  class:dimmed={!active}
  class:breathing={effectClass === 'breathing'}
  class:ripple={effectClass === 'ripple'}
  class:pulse={effectClass === 'pulse'}
  class:shake={effectClass === 'shake'}
  class:wakeup={effectClass === 'wakeup'}
  style="width: {size}px; height: {size}px; --glow-c: {glowColor};"
>
  <svg viewBox="0 0 110 110" class="ring-svg" width={size} height={size}>
    <defs>
      <linearGradient id="grad-outer-{uid()}" x1="0%" y1="0%" x2="100%" y2="100%">
        <stop offset="0%" stop-color={ringColors.outer} stop-opacity="0.25" />
        <stop offset="100%" stop-color={ringColors.outer} stop-opacity="0.6" />
      </linearGradient>
      <linearGradient id="grad-inner-{uid()}" x1="0%" y1="0%" x2="100%" y2="100%">
        <stop offset="0%" stop-color={ringColors.inner} stop-opacity="0.25" />
        <stop offset="100%" stop-color={ringColors.inner} stop-opacity="0.5" />
      </linearGradient>
    </defs>

    <!-- track rings — barely there -->
    <circle cx="55" cy="55" r={R_OUTER} fill="none"
      stroke="var(--border)" stroke-width="1.5" opacity="0.5" />
    <circle cx="55" cy="55" r={R_INNER} fill="none"
      stroke="var(--border)" stroke-width="1.5" opacity="0.5" />

    <!-- outer ring (progress) -->
    <circle cx="55" cy="55" r={R_OUTER} fill="none"
      stroke="url(#grad-outer-{uid()})" stroke-width="2"
      stroke-dasharray={C_OUTER} stroke-dashoffset={outerDash}
      stroke-linecap="round" transform="rotate(-90 55 55)" />

    <!-- inner ring (secondary) -->
    <circle cx="55" cy="55" r={R_INNER} fill="none"
      stroke="url(#grad-inner-{uid()})" stroke-width="2"
      stroke-dasharray={C_INNER} stroke-dashoffset={innerDash}
      stroke-linecap="round" transform="rotate(-90 55 55)" />
  </svg>

  <div class="ring-center" style="font-size: {46 * scale}px;">
    {#if imageSrc}
      <img class="ring-emotion-img" src={imageSrc} alt="" />
    {:else}
      {emoji}
    {/if}
  </div>
</div>

{#if showLabel}
  <div class="label-text">{label}</div>
{/if}

{#if showInfo}
  <div class="info-lines">
    {#if info1}
      <span class="info-outer" style="color: {ringColors.outer}">{info1}</span>
    {/if}
    {#if info2}
      <span class="info-inner" style="color: {ringColors.inner}">{info2}</span>
    {/if}
  </div>
{/if}

<style>
  .idle-ring {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    overflow: visible; /* allow ripple pseudo-elements to extend beyond bounds */
  }
  .idle-ring.dimmed {
    opacity: 0.35;
  }
  .ring-svg {
    display: block;
    filter: drop-shadow(0 0 4px color-mix(in srgb, var(--glow-c, #6c8cff) 10%, transparent));
    transition: filter 0.5s;
    /* keep SVG above pseudo-element ripples so rings stay crisp */
    position: relative;
    z-index: 1;
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
  .label-text {
    font-size: 10px;
    color: var(--fg-dim);
    letter-spacing: 1.5px;
    text-align: center;
    margin-top: 6px;
  }
  .info-lines {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    margin-top: 4px;
  }
  .info-outer, .info-inner {
    font-size: 10px;
    font-weight: 700;
    line-height: 1.5;
  }

  /* ── Breathing (idle / wakeup) ────────────────────────────────────────
     Subtle scale pulse — the rings stay perfectly readable, the whole
     component just "breathes" at ~8 s per cycle. */

  .idle-ring.breathing {
    animation: breathe 8s ease-in-out infinite;
  }

  @keyframes breathe {
    0%, 100% { transform: scale(1); }
    50%      { transform: scale(1.025); }
  }

  /* ── Ripple (reflection / processing) ─────────────────────────────────
     Two staggered border-only rings expand outward from just beyond the
     outer progress ring.  Because the pseudo-elements have no background
     fill, the inner rings and centre image are never obscured. */

  .idle-ring.ripple::before,
  .idle-ring.ripple::after {
    content: "";
    position: absolute;
    inset: -20%;
    border-radius: 50%;
    border: 1.2px solid var(--glow-c, #6c8cff);
    opacity: 0;
    pointer-events: none;
  }

  .idle-ring.ripple::before {
    animation: ripple-expand 2.8s ease-out infinite;
  }

  .idle-ring.ripple::after {
    animation: ripple-expand 2.8s ease-out 1.4s infinite;
  }

  @keyframes ripple-expand {
    0% {
      transform: scale(0.65);
      opacity: 0.45;
    }
    100% {
      transform: scale(1.05);
      opacity: 0;
    }
  }

  /* ── Pulse (task completed) ──────────────────────────────────────────
     Brief glow intensify on the ring SVG, then settle back. */

  .idle-ring.pulse .ring-svg {
    animation: ring-pulse 0.5s ease-out;
  }

  @keyframes ring-pulse {
    0% {
      filter: drop-shadow(0 0 18px color-mix(in srgb, var(--glow-c, #6c8cff) 90%, transparent));
    }
    100% {
      filter: drop-shadow(0 0 4px color-mix(in srgb, var(--glow-c, #6c8cff) 10%, transparent));
    }
  }

  /* ── Shake (error) ─────────────────────────────────────────────────── */

  .idle-ring.shake {
    animation: ring-shake 0.35s ease-out;
  }

  @keyframes ring-shake {
    0%, 100% { transform: translateX(0); }
    12%      { transform: translateX(-5px); }
    25%      { transform: translateX(5px); }
    37%      { transform: translateX(-4px); }
    50%      { transform: translateX(4px); }
    62%      { transform: translateX(-2px); }
    75%      { transform: translateX(2px); }
    87%      { transform: translateX(-1px); }
  }

  /* ── Wake-up (idle → active transition) ──────────────────────────────
     Quick "eyes opening": shrink slightly, then expand just past resting
     size before settling. */

  .idle-ring.wakeup {
    animation: ring-wakeup 0.7s ease-out;
  }

  @keyframes ring-wakeup {
    0%   { transform: scale(0.88); }
    35%  { transform: scale(1.06); }
    70%  { transform: scale(0.98); }
    100% { transform: scale(1); }
  }

  /* ── Accessibility ─────────────────────────────────────────────────── */

  @media (prefers-reduced-motion: reduce) {
    .idle-ring.breathing,
    .idle-ring.ripple::before,
    .idle-ring.ripple::after,
    .idle-ring.pulse .ring-svg,
    .idle-ring.shake,
    .idle-ring.wakeup {
      animation: none;
    }
  }
</style>
