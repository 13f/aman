// Shared types for agent viewer components (grid + aoa-realm).

export interface AgentEntry {
  key: string;
  display_name: string;
  provider: string;
  model: string;
  soul_summary: string;
  session_count: number;
  is_active: boolean;
}

export interface AgentIdleState {
  mode: "idle" | "reflection" | "processing";
  outerPct: number;
  innerPct: number;
  emoji: string;
  kind: string;
}

export interface TiltState {
  tiltX: number;
  tiltY: number;
  glossX: number;
  glossY: number;
  hovering: boolean;
}

export interface AgentGridViewEvents {
  onSelect: (agent: AgentEntry) => void;
}

export const COLORS: Record<string, { outer: string; inner: string }> = {
  idle:       { outer: "#6c8cff", inner: "#f59e0b" },
  reflection: { outer: "#a78bfa", inner: "#f472b6" },
  processing: { outer: "#4ade80", inner: "#22d3ee" },
};

export const IDLE_EMOJI: Record<string, string> = {
  daze: "\u{1F636}", boredom: "\u{1F612}", sleep: "\u{1F634}",
  exploration: "\u{1F50D}", meditation: "\u{1F9D8}",
  incubation: "\u{1F4A1}", waiting: "\u{23F3}",
};

export const MODE_ICON: Record<string, string> = {
  idle: "\u{1F4A4}", reflection: "\u{1F9E0}", processing: "\u{26A1}",
};

export const STATE_EMOJI: Record<string, string> = {
  working: "\u{1F6E0}\u{FE0F}",
  studying: "\u{1F4DA}",
  daily_life: "\u{1F3E0}",
  prize: "\u{1F3C6}",
  waiting: "\u{23F3}",
};

export const SYSTEM_STATE_LABEL: Record<string, string> = {
  idle: "Idle",
  preparing: "Loading",
  working: "Working",
  chatting: "Chatting",
  studying: "Studying",
  daily_life: "Daily Life",
  prize: "Prize",
  waiting: "Waiting",
};

export const SYSTEM_STATE_CLASS: Record<string, string> = {
  idle: "ss-idle",
  preparing: "ss-loading",
  working: "ss-working",
  chatting: "ss-chatting",
  studying: "ss-studying",
  daily_life: "ss-dailylife",
  prize: "ss-prize",
  waiting: "ss-waiting",
};
