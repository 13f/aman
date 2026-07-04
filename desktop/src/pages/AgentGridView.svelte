<script lang="ts">
  import { fade } from "svelte/transition";
  import IdleRing from "./IdleRing.svelte";
  import CognitiveRing from "./CognitiveRing.svelte";
  import CognitiveAura from "./CognitiveAura.svelte";
  import { resolveEmotionImage } from "../lib/emotions";
  import type { EmotionsConfig } from "../lib/emotions";
  import type { CognitiveState } from "../lib/cognitive-state";
  import {
    type AgentEntry,
    type AgentIdleState,
    type TiltState,
    type AgentGridViewEvents,
    COLORS,
    MODE_ICON,
    STATE_EMOJI,
    SYSTEM_STATE_LABEL,
    SYSTEM_STATE_CLASS,
  } from "./agent-viewer-types";

  let {
    agents = [],
    idleStates = {} as Record<string, AgentIdleState>,
    systemStates = {} as Record<string, string>,
    emotionsConfigs = {} as Record<string, EmotionsConfig | null>,
    cognitiveStates = {} as Record<string, CognitiveState>,
    // CognitiveState (LLM backend health): "Lucid" | "Groggy" | "Catatonic" | "Coma"
    brainStates = {} as Record<string, string>,
    onSelect = (_agent: AgentEntry) => {},
    prefersReducedMotion = false,
  }: AgentGridViewEvents & {
    agents: AgentEntry[];
    idleStates?: Record<string, AgentIdleState>;
    systemStates?: Record<string, string>;
    emotionsConfigs?: Record<string, EmotionsConfig | null>;
    cognitiveStates?: Record<string, CognitiveState>;
    brainStates?: Record<string, string>;
    prefersReducedMotion?: boolean;
  } = $props();

  // ── 3D Tilt state ──────────────────────────────────────────────────────
  let tiltStates = $state<Record<string, TiltState>>({});

  function getTilt(key: string): TiltState {
    return tiltStates[key] ?? { tiltX: 0, tiltY: 0, glossX: 50, glossY: 50, hovering: false };
  }

  function handleTilt(key: string, e: MouseEvent) {
    if (prefersReducedMotion) return;
    const card = e.currentTarget as HTMLElement;
    const rect = card.getBoundingClientRect();
    const centerX = rect.left + rect.width / 2;
    const centerY = rect.top + rect.height / 2;
    const mouseX = e.clientX - centerX;
    const mouseY = e.clientY - centerY;
    const tiltY = (mouseX / (rect.width / 2)) * 10;
    const tiltX = -(mouseY / (rect.height / 2)) * 10;
    const glossX = ((e.clientX - rect.left) / rect.width) * 100;
    const glossY = ((e.clientY - rect.top) / rect.height) * 100;
    tiltStates = {
      ...tiltStates,
      [key]: { tiltX, tiltY, glossX, glossY, hovering: true },
    };
  }

  function handleTiltLeave(key: string) {
    tiltStates = {
      ...tiltStates,
      [key]: { tiltX: 0, tiltY: 0, glossX: 50, glossY: 50, hovering: false },
    };
  }

  function getIdleState(key: string): AgentIdleState {
    return idleStates[key] ?? defaultIdleState();
  }

  function defaultIdleState(): AgentIdleState {
    return { mode: "idle", outerPct: 0, innerPct: 0, emoji: MODE_ICON.idle, kind: "" };
  }

  function getSystemState(key: string): string {
    return systemStates[key] ?? "idle";
  }

  function defaultCognitiveState(): CognitiveState {
    return { phase: "idle", currentStep: "" };
  }

  function getCognitiveState(key: string): CognitiveState {
    return cognitiveStates[key] ?? defaultCognitiveState();
  }

  function getEmotionImage(key: string, stateOrKind: string): string {
    const cfg = emotionsConfigs[key];
    const fromState = resolveEmotionImage(cfg, stateOrKind);
    if (fromState) return fromState;
    return "";
  }
</script>

{#if agents.length === 0}
  <div class="card" style="text-align:center;padding:40px;">
    <p>No agents yet.</p>
    <p class="dim" style="margin-top:8px;">Go to Services → Agents to create your first agent.</p>
  </div>
{:else}
  <div class="agent-grid">
    {#each agents as agent}
      {@const st = getIdleState(agent.key)}
      {@const ss = getSystemState(agent.key)}
      {@const ts = getTilt(agent.key)}
      <button
        class="agent-avatar-card"
        class:needs-config={!agent.provider}
        onclick={() => onSelect(agent)}
        onmouseenter={(e) => handleTilt(agent.key, e)}
        onmousemove={(e) => handleTilt(agent.key, e)}
        onmouseleave={() => handleTiltLeave(agent.key)}
        style="--tilt-x: {ts.tiltX}deg; --tilt-y: {ts.tiltY}deg; --gloss-x: {ts.glossX}%; --gloss-y: {ts.glossY}%;"
      >
        <div class="agent-avatar-wrap">
        {#if (brainStates[agent.key] ?? "Lucid") !== "Lucid" && agent.provider}
          <!-- Layer 1 (highest priority): LLM backend degraded → CognitiveAura -->
          {@const bs = brainStates[agent.key] ?? "Groggy"}
          {@const imgSrc = getEmotionImage(agent.key, "idle")}
          <div transition:fade={{ duration: 400 }}>
            <CognitiveAura
              state={bs as "Groggy" | "Catatonic" | "Coma"}
              emoji={bs === "Groggy" ? "\u{1F97A}" : bs === "Catatonic" ? "\u{1F636}" : "\u{1F4A4}"}
              imageSrc={imgSrc}
              size={165}
              active={true}
            />
          </div>
        {:else if ss === "idle" || !agent.provider}
          <!-- Layer 2: idle aura (LLM healthy) -->
          {@const imgSrc = getEmotionImage(agent.key, st.kind || "idle")}
          <div transition:fade={{ duration: 300 }}>
            <IdleRing
              mode={st.mode}
              outerPct={st.outerPct}
              innerPct={st.innerPct}
              emoji={st.emoji}
              imageSrc={imgSrc}
              ringColors={COLORS[st.mode]}
              size={165}
              showLabel={false}
              showInfo={false}
              active={!!agent.provider}
            />
          </div>
        {:else}
          <!-- Layer 3: active ReAct phase ring (LLM healthy + working) -->
          {@const cs = getCognitiveState(agent.key)}
          {@const imgSrc = getEmotionImage(agent.key, ss)}
          {@const phaseEmoji = cs.phase === "observing" ? "\u{1F50D}" :
                               cs.phase === "thinking"  ? "\u{1F9E0}" :
                               cs.phase === "acting"    ? "\u{1F6E0}\u{FE0F}" :
                               cs.phase === "result"    ? "\u{2705}" :
                               STATE_EMOJI[ss] ?? "\u{1F4CB}"}
          <!-- CognitiveRing 只在 ReAct 相位不是 idle 时显示（避免 DailyLife 等非 LLM 状态显示全暗）。
               Phase 为 idle 时回退到 IdleRing（显示 processing 模式）。 -->
          <div transition:fade={{ duration: 300 }}>
            {#if cs.phase !== "idle"}
            <CognitiveRing
              reactPhase={cs.phase}
              currentStep={cs.currentStep}
              emoji={phaseEmoji}
              imageSrc={imgSrc}
              size={165}
              active={!!agent.provider}
            />
            {:else}
            <IdleRing
              mode="processing"
              outerPct={50}
              innerPct={50}
              emoji={STATE_EMOJI[ss] ?? MODE_ICON.processing}
              imageSrc={imgSrc}
              ringColors={COLORS["processing"]}
              size={165}
              showLabel={false}
              showInfo={false}
              active={!!agent.provider}
            />
            {/if}
          </div>
        {/if}
        </div>
        <div class="agent-card-info">
          <span class="agent-avatar-name">{agent.display_name}</span>
          {#if !agent.provider}
            <span class="badge warn" style="font-size:10px;">needs config</span>
          {:else}
            <span class="agent-status-row {SYSTEM_STATE_CLASS[ss] ?? 'ss-idle'}">
              <span class="agent-status-dot"></span>
              {SYSTEM_STATE_LABEL[ss] ?? ss}
            </span>
          {/if}
        </div>
      </button>
    {/each}
  </div>
{/if}

<style>
  .agent-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
    gap: 28px;
    padding-top: 12px;
  }

  .agent-avatar-wrap {
    filter: drop-shadow(0 8px 24px rgba(0, 0, 0, 0.5));
    transition: transform 0.25s ease, filter 0.25s;
  }

  .agent-avatar-card:hover .agent-avatar-wrap {
    transform: scale(1.06);
    filter: drop-shadow(0 12px 32px rgba(0, 0, 0, 0.6));
  }

  .agent-avatar-card {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0;
    padding: 0 8px 8px;
    background: none;
    border: none;
    border-radius: 0;
    cursor: pointer;
    transform: perspective(800px) rotateX(var(--tilt-x, 0deg)) rotateY(var(--tilt-y, 0deg));
    transition: transform 0.6s cubic-bezier(0.23, 1, 0.32, 1);
    transform-style: preserve-3d;
    text-align: center;
    box-shadow: none;
  }

  .agent-avatar-card:hover {
    transform: perspective(800px) rotateX(var(--tilt-x, 0deg)) rotateY(var(--tilt-y, 0deg)) translateY(-4px);
  }

  .agent-avatar-card::after {
    content: "";
    position: absolute;
    inset: -20px;
    border-radius: 50%;
    pointer-events: none;
    z-index: 5;
    opacity: 0;
    transition: opacity 0.4s;
    background: radial-gradient(
      circle at var(--gloss-x, 50%) var(--gloss-y, 50%),
      rgba(255, 255, 255, 0.10) 0%,
      rgba(255, 255, 255, 0.03) 30%,
      transparent 60%
    );
  }

  .agent-avatar-card:hover::after {
    opacity: 1;
  }

  .agent-avatar-card.needs-config {
    opacity: 0.45;
    cursor: pointer;
  }

  .agent-avatar-card.needs-config:hover {
    opacity: 0.65;
  }

  .agent-card-info {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 6px;
    margin-top: 12px;
  }

  .agent-avatar-name {
    font-size: 15px;
    font-weight: 700;
    color: var(--fg);
    line-height: 1.3;
    overflow-wrap: break-word;
    letter-spacing: 0.01em;
  }

  .agent-status-row {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    font-weight: 500;
    transition: color 0.3s;
    line-height: 1;
  }
  .agent-status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
    transition: background 0.3s, box-shadow 0.3s;
  }

  .ss-idle .agent-status-dot,
  .ss-idle { color: var(--accent); }
  .ss-idle .agent-status-dot { background: var(--accent); }

  .ss-working .agent-status-dot,
  .ss-working { color: var(--green); }
  .ss-working .agent-status-dot {
    background: var(--green);
    box-shadow: 0 0 8px color-mix(in srgb, var(--green) 60%, transparent);
    animation: statusPulse 2s ease-in-out infinite;
  }

  .ss-studying .agent-status-dot,
  .ss-studying { color: #b39dfc; }
  .ss-studying .agent-status-dot {
    background: #b39dfc;
    box-shadow: 0 0 8px rgba(167, 139, 250, 0.5);
    animation: statusPulse 3s ease-in-out infinite;
  }

  .ss-dailylife .agent-status-dot,
  .ss-dailylife { color: var(--yellow); }
  .ss-dailylife .agent-status-dot {
    background: var(--yellow);
    box-shadow: 0 0 8px color-mix(in srgb, var(--yellow) 50%, transparent);
    animation: statusPulse 3.5s ease-in-out infinite;
  }

  .ss-waiting .agent-status-dot,
  .ss-waiting { color: #f59e0b; }
  .ss-waiting .agent-status-dot {
    background: #f59e0b;
    box-shadow: 0 0 8px rgba(245, 158, 11, 0.5);
    animation: statusPulse 2.5s ease-in-out infinite;
  }

  .ss-chatting .agent-status-dot,
  .ss-chatting { color: #38dff0; }
  .ss-chatting .agent-status-dot {
    background: #38dff0;
    box-shadow: 0 0 8px rgba(34, 211, 238, 0.5);
    animation: statusPulse 1.5s ease-in-out infinite;
  }

  @keyframes statusPulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.55; transform: scale(1.35); }
  }

  .dim { color: var(--fg-dim); }
  .card { background: var(--bg-card); border: 1px solid var(--border); border-radius: 12px; }

  .avatar-pose,
  .agent-avatar-wrap :global(.ring-center),
  .agent-avatar-wrap :global(.ring-emotion-img) {
    animation: agentPose 6s ease-in-out infinite;
  }

  @keyframes agentPose {
    0%, 100% { transform: translateY(0) rotate(0deg); }
    12%  { transform: translateY(-3px) rotate(0.4deg); }
    25%  { transform: translateY(-1px) rotate(-0.3deg); }
    37%  { transform: translateY(-3.5px) rotate(0deg); }
    50%  { transform: translateY(1.5px) rotate(0.3deg); }
    62%  { transform: translateY(0) rotate(-0.2deg); }
    75%  { transform: translateY(-2px) rotate(0.2deg); }
    87%  { transform: translateY(-0.5px) rotate(0deg); }
  }

  @media (prefers-reduced-motion: reduce) {
    .agent-avatar-card {
      transform: none;
      transition: none;
    }
    .agent-avatar-card:hover {
      transform: translateY(-4px);
    }
    .agent-avatar-card::after {
      display: none;
    }
    .avatar-pose,
    .agent-avatar-wrap :global(.ring-center),
    .agent-avatar-wrap :global(.ring-emotion-img) {
      animation: none;
    }
  }
</style>
