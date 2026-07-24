<script lang="ts">
  // CognitiveState aura — rendered when LLM backend is degraded.
  // Visual language: "unhealthy" — murky, stagnant, cold.
  // Three states: Groggy / Catatonic / Coma (Lucid is transparent, never rendered here).

  type CognitiveAuraState = "Groggy" | "Catatonic" | "Coma";

  // Stable unique id per component instance for SVG gradient references.
  let _uid = "";
  function uid(): string {
    if (!_uid) _uid = "ca" + Math.random().toString(36).slice(2, 8);
    return _uid;
  }

  let {
    state = "Groggy" as CognitiveAuraState,
    arousal = undefined as number | undefined,
    emoji = "",
    imageSrc = "",
    size = 165,
    active = true,
  }: {
    state?: CognitiveAuraState;
    /** 外部传入的 arousal 值。不传时根据 state 自动推导。 */
    arousal?: number;
    emoji?: string;
    imageSrc?: string;
    size?: number;
    active?: boolean;
  } = $props();

  const R_OUTER = 48;
  const R_INNER = 41;
  const C_OUTER = 2 * Math.PI * R_OUTER;
  const C_INNER = 2 * Math.PI * R_INNER;

  function dash(circum: number, pct: number): number {
    const clamped = Math.min(100, Math.max(1, pct));
    return circum - (clamped / 100) * circum;
  }

  // ── State → visual mapping ──────────────────────────────────────────────

  // Arousal: 外部未传时根据 state 自动推导（与 gateway 侧 CognitiveMonitor 保持一致）
  let effectiveArousal = $derived(
    arousal ??
    (state === "Groggy" ? 0.3 :
     state === "Catatonic" ? 0.05 :
     0.0)  // Coma
  );

  // Outer ring color: the "body" of the aura.
  let outerColor = $derived(
    state === "Groggy" ? "#94782e" :      // murky amber
    state === "Catatonic" ? "#6b7280" :   // cold gray
    "#3b1d6e"                              // deep purple (Coma)
  );

  // Inner ring color: dimmer variant.
  let innerColor = $derived(
    state === "Groggy" ? "#6b5520" :
    state === "Catatonic" ? "#4b5563" :
    "#2a1450"
  );

  // Glow intensity: fades as state worsens.
  let glowIntensity = $derived(
    state === "Groggy" ? 0.15 :
    state === "Catatonic" ? 0.06 :
    0.02                                   // Coma: barely visible
  );

  // Breathing animation: slows and weakens as state worsens.
  let breatheDuration = $derived(
    state === "Groggy" ? "18s" :
    state === "Catatonic" ? "30s" :
    "0s"                                   // Coma: no breathing
  );
  let breatheAmplitude = $derived(
    state === "Groggy" ? 1.015 :
    state === "Catatonic" ? 1.005 :
    1.0
  );

  // Outer ring progress: encodes arousal (0-100).
  let outerPct = $derived(Math.round(effectiveArousal * 100));
  let outerDash = $derived(dash(C_OUTER, outerPct));
  let innerDash = $derived(dash(C_INNER, 100)); // inner always full

  let scale = $derived(size / 110);

  // Emoji fallback per state.
  let displayEmoji = $derived(
    emoji ||
    (state === "Groggy" ? "\u{1F97A}" :      // dizzy face
     state === "Catatonic" ? "\u{1F636}" :   // face without mouth
     "\u{1F4A4}")                            // zzz (Coma)
  );
</script>

<div
  class="cognitive-aura"
  class:comatose={state === "Coma"}
  class:dimmed={!active}
  style="width: {size}px; height: {size}px; --glow-c: {outerColor}; --glow-intensity: {glowIntensity}; --breathe-dur: {breatheDuration}; --breathe-amp: {breatheAmplitude};"
>
  <svg viewBox="0 0 110 110" class="aura-svg" width={size} height={size}>
    <defs>
      <linearGradient id="cog-grad-outer-{uid()}" x1="0%" y1="0%" x2="100%" y2="100%">
        <stop offset="0%" stop-color={outerColor} stop-opacity="0.15" />
        <stop offset="100%" stop-color={outerColor} stop-opacity="0.45" />
      </linearGradient>
      <linearGradient id="cog-grad-inner-{uid()}" x1="0%" y1="0%" x2="100%" y2="100%">
        <stop offset="0%" stop-color={innerColor} stop-opacity="0.10" />
        <stop offset="100%" stop-color={innerColor} stop-opacity="0.30" />
      </linearGradient>
    </defs>

    <!-- track rings -->
    <circle cx="55" cy="55" r={R_OUTER} fill="none"
      stroke="var(--border)" stroke-width="1.5" opacity="0.3" />
    <circle cx="55" cy="55" r={R_INNER} fill="none"
      stroke="var(--border)" stroke-width="1.5" opacity="0.3" />

    <!-- outer ring (arousal progress) -->
    <circle cx="55" cy="55" r={R_OUTER} fill="none"
      stroke="url(#cog-grad-outer-{uid()})" stroke-width="2"
      stroke-dasharray={C_OUTER} stroke-dashoffset={outerDash}
      stroke-linecap="round" transform="rotate(-90 55 55)" />

    <!-- inner ring (always full, dim) -->
    <circle cx="55" cy="55" r={R_INNER} fill="none"
      stroke="url(#cog-grad-inner-{uid()})" stroke-width="2"
      stroke-dasharray={C_INNER} stroke-dashoffset={innerDash}
      stroke-linecap="round" transform="rotate(-90 55 55)" />

    <!-- Coma: faint static pulse ring -->
    {#if state === "Coma"}
      <circle cx="55" cy="55" r="52" fill="none"
        stroke={outerColor} stroke-width="0.5" opacity="0.15" />
    {/if}
  </svg>

  <div class="aura-center" style="font-size: {46 * scale}px;">
    {#if imageSrc}
      <img class="aura-emotion-img" src={imageSrc} alt="" />
    {:else}
      {displayEmoji}
    {/if}
  </div>
</div>

<style>
  .cognitive-aura {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    overflow: visible;
  }

  .cognitive-aura.dimmed {
    opacity: 0.35;
  }

  .aura-svg {
    display: block;
    filter: drop-shadow(0 0 calc(4px * var(--glow-intensity, 0.1))
      color-mix(in srgb, var(--glow-c, #6b7280) calc(var(--glow-intensity, 0.1) * 100%), transparent));
    transition: filter 0.8s ease;
    position: relative;
    z-index: 1;
  }

  .aura-center {
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

  .aura-emotion-img {
    width: 65%;
    height: 65%;
    object-fit: contain;
    border-radius: 50%;
    pointer-events: none;
    box-shadow: 0 4px 20px rgba(0, 0, 0, 0.4);
  }

  /* ── Breathing (Groggy / Catatonic) ───────────────────────────────────
     Slower and weaker than IdleRing's breathing — conveys "struggling". */

  .cognitive-aura:not(.comatose) {
    animation: cog-breathe var(--breathe-dur, 18s) ease-in-out infinite;
  }

  @keyframes cog-breathe {
    0%, 100% { transform: scale(1); }
    50%      { transform: scale(var(--breathe-amp, 1.015)); }
  }

  /* ── Coma: no breathing, just a faint static presence ───────────────── */

  .cognitive-aura.comatose {
    /* No animation — the aura is "dead" */
    opacity: 0.6;
  }

  /* ── Accessibility ─────────────────────────────────────────────────── */

  @media (prefers-reduced-motion: reduce) {
    .cognitive-aura:not(.comatose) {
      animation: none;
    }
  }
</style>
