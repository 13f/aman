<script lang="ts">
  type Mode = "idle" | "reflection" | "processing";

  let {
    mode = "idle" as Mode,
    outerPct = 0,
    innerPct = 0,
    emoji = "",
    label = "",
    info1 = "",
    info2 = "",
    ringColors = { outer: "#6c8cff", inner: "#f59e0b" },
    size = 110,
    active = true,
    showLabel = true,
    showInfo = true,
  }: {
    mode?: Mode;
    outerPct?: number;
    innerPct?: number;
    emoji?: string;
    label?: string;
    info1?: string;
    info2?: string;
    ringColors?: { outer: string; inner: string };
    size?: number;
    active?: boolean;
    showLabel?: boolean;
    showInfo?: boolean;
  } = $props();

  const R_OUTER = 48;
  const R_INNER = 34;
  const C_OUTER = 2 * Math.PI * R_OUTER;
  const C_INNER = 2 * Math.PI * R_INNER;

  function dash(circum: number, pct: number): number {
    return circum - (Math.min(100, Math.max(0, pct)) / 100) * circum;
  }

  let outerDash = $derived(dash(C_OUTER, outerPct));
  let innerDash = $derived(dash(C_INNER, innerPct));
  let scale = $derived(size / 110);
</script>

<div
  class="idle-ring"
  class:dimmed={!active}
  style="width: {size}px; height: {size}px;"
>
  <svg viewBox="0 0 110 110" class="ring-svg" width={size} height={size}>
    <!-- track rings -->
    <circle cx="55" cy="55" r={R_OUTER} fill="none" stroke="var(--border)" stroke-width="5" />
    <circle cx="55" cy="55" r={R_INNER} fill="none" stroke="var(--border)" stroke-width="5" />

    <!-- outer ring (progress) -->
    <circle cx="55" cy="55" r={R_OUTER} fill="none" stroke={ringColors.outer} stroke-width="5"
      stroke-dasharray={C_OUTER} stroke-dashoffset={outerDash}
      stroke-linecap="round" transform="rotate(-90 55 55)" />

    <!-- inner ring (secondary) -->
    <circle cx="55" cy="55" r={R_INNER} fill="none" stroke={ringColors.inner} stroke-width="5"
      stroke-dasharray={C_INNER} stroke-dashoffset={innerDash}
      stroke-linecap="round" transform="rotate(-90 55 55)" />
  </svg>

  <div class="ring-center" style="font-size: {30 * scale}px;">{emoji}</div>
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
  }
  .idle-ring.dimmed {
    opacity: 0.35;
  }
  .ring-svg {
    display: block;
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
</style>
