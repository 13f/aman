<script lang="ts">
  import type { ReactPhase } from "../lib/cognitive-state";
  import { PHASE_COLORS } from "../lib/cognitive-state";

  let {
    reactPhase = "idle" as ReactPhase,
    currentStep = "",
    emoji = "",
    imageSrc = "",
    size = 110,
    active = true,
  }: {
    reactPhase?: ReactPhase;
    currentStep?: string;
    emoji?: string;
    imageSrc?: string;
    size?: number;
    active?: boolean;
  } = $props();

  const R = 44;
  const SW = 2.5;
  const CIRCUM = 2 * Math.PI * R; // ~276.46
  const SEG_LEN = CIRCUM / 4;      // ~69.115
  const GAP = CIRCUM - SEG_LEN;   // ~207.345

  let scale = $derived(size / 110);

  /** 4 segments, clockwise from 12-o'clock. Each is a full circle with
   *  stroke-dasharray showing exactly one quarter. */
  const SEGMENTS: Array<{ phase: ReactPhase; color: string; offset: number }> = [
    { phase: "observing", color: PHASE_COLORS["observing"], offset: 0 },
    { phase: "thinking",  color: PHASE_COLORS["thinking"],  offset: -SEG_LEN },
    { phase: "acting",    color: PHASE_COLORS["acting"],    offset: -2 * SEG_LEN },
    { phase: "result",    color: PHASE_COLORS["result"],    offset: -3 * SEG_LEN },
  ];

  /** Accessible label for the current phase. */
  const PHASE_LABEL: Record<ReactPhase, string> = {
    observing: "Observing",
    thinking: "Thinking",
    acting: "Acting",
    result: "Result",
    idle: "Idle",
  };

  /** Reduced-motion preference.  Read once at mount — the component is
   *  lightweight enough that we don't need a live watcher. */
  let prefersReducedMotion = $state(false);
  $effect(() => {
    prefersReducedMotion = window.matchMedia(
      "(prefers-reduced-motion: reduce)",
    ).matches;
  });
</script>

<div
  class="cognitive-ring"
  class:dimmed={!active}
  class:no-transition={prefersReducedMotion}
  style="width: {size}px; height: {size}px;"
  role="img"
  aria-label="Cognitive state: {PHASE_LABEL[reactPhase]}{currentStep ? ' — ' + currentStep : ''}"
>
  <svg viewBox="0 0 110 110" class="ring-svg" width={size} height={size}>
    <!-- faint full track ring behind the segments -->
    <circle
      cx="55" cy="55" r={R}
      fill="none"
      stroke="var(--border)"
      stroke-width={SW}
      opacity="0.2"
    />

    {#each SEGMENTS as seg}
      <circle
        cx="55" cy="55" r={R}
        fill="none"
        stroke={seg.color}
        stroke-width={SW}
        stroke-linecap="butt"
        stroke-dasharray="{SEG_LEN} {GAP}"
        stroke-dashoffset={seg.offset}
        transform="rotate(-90 55 55)"
        class="seg"
        class:lit={reactPhase === seg.phase}
      />
    {/each}
  </svg>

  <div class="ring-center" style="font-size: {46 * scale}px;">
    {#if imageSrc}
      <img class="ring-emotion-img" src={imageSrc} alt="" />
    {:else}
      {emoji}
    {/if}
  </div>
</div>

{#if currentStep && active}
  <div class="step-text" style="max-width: {size}px;" class:no-transition={prefersReducedMotion}>
    {currentStep}
  </div>
{/if}

<style>
  .cognitive-ring {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
  }
  .cognitive-ring.dimmed {
    opacity: 0.35;
  }

  .ring-svg {
    display: block;
    position: relative;
    z-index: 1;
    filter: drop-shadow(0 0 4px color-mix(in srgb, var(--fg-dim, #8892b0) 8%, transparent));
  }

  /* ── segment circles ────────────────────────────────────────────── */

  .seg {
    opacity: 0.22;
    transition:
      opacity 0.6s ease,
      stroke-dashoffset 0.6s ease;
  }
  .seg.lit {
    opacity: 1;
  }
  .no-transition .seg {
    transition: none;
  }

  /* ── centre content (same geometry as IdleRing) ─────────────────── */

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

  /* ── step text ──────────────────────────────────────────────────── */

  .step-text {
    font-size: 11px;
    color: var(--fg-dim);
    opacity: 0.7;
    text-align: center;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    margin-top: 5px;
    transition: opacity 0.4s ease;
  }
  .no-transition.step-text {
    transition: none;
  }

  /* ── accessibility ──────────────────────────────────────────────── */

  @media (prefers-reduced-motion: reduce) {
    .seg {
      transition: none;
    }
    .step-text {
      transition: none;
    }
  }
</style>
