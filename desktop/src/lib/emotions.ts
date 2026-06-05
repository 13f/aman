import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// Types — mirror the Rust models in desktop/src/models.rs
// ---------------------------------------------------------------------------

export interface EmotionEntry {
  id: string;
  tags: string[];
  description: string;
  /** Base64-encoded data URL ready for `<img src="...">`. */
  data_url: string;
}

export interface EmotionsConfig {
  img_ext: string;
  items: EmotionEntry[];
}

// ---------------------------------------------------------------------------
// State → emotion‑id mapping
// ---------------------------------------------------------------------------
//
// Maps the app's internal state keys (idle sub‑modes, system states, and
// transient modes) to emotion IDs defined in the agent's emotions/data.json.
// When the gateway / LLM starts sending explicit emotion IDs via events this
// mapping can still serve as a sensible fallback.

const STATE_TO_EMOTION: Record<string, string> = {
  // Idle sub-modes
  daze: "daze",
  boredom: "bored",
  sleep: "sleeping",
  exploration: "curious",
  meditation: "calm",
  incubation: "thinking",
  waiting: "waiting",
  // Bare idle (no sub-mode yet) — calm/neutral
  idle: "calm",
  // Active system states
  working: "working",
  chatting: "happy",
  studying: "studying",
  daily_life: "relaxed",
  // Transient / local modes
  processing: "focused",
  reflection: "thinking",
};

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

const cache = new Map<string, EmotionsConfig | null>();

/** Load (or return cached) emotions config for a given agent key.
 *  Returns `null` when emotions are unavailable — the caller should fall
 *  back to the default emoji display. */
export async function loadEmotions(
  agentKey: string,
): Promise<EmotionsConfig | null> {
  if (cache.has(agentKey)) {
    return cache.get(agentKey)!;
  }

  try {
    const cfg: EmotionsConfig | null = await invoke("get_agent_emotions", {
      key: agentKey,
    });
    cache.set(agentKey, cfg);
    return cfg;
  } catch {
    cache.set(agentKey, null);
    return null;
  }
}

/**
 * Look up the data URL for the emotion image that best represents `stateOrKind`.
 *
 * `stateOrKind` can be a top-level system state (`"working"`, `"studying"`, …),
 * an idle sub-mode kind (`"daze"`, `"sleep"`, …), a transient mode
 * (`"processing"`, `"reflection"`), or a bare emotion ID when the
 * gateway/LLM provides one directly.
 *
 * Returns `null` when:
 * - `config` is `null` (emotions not configured for this agent),
 * - `stateOrKind` doesn't map to a known emotion ID, or
 * - the emotion ID isn't present in the config.
 */
export function resolveEmotionImage(
  config: EmotionsConfig | null,
  stateOrKind: string,
): string | null {
  if (!config) return null;

  // Try a direct lookup first (supports bare emotion IDs from the gateway).
  let emotionId = STATE_TO_EMOTION[stateOrKind] ?? null;

  // If no direct mapping, try tag-based fuzzy match.
  if (!emotionId) {
    const lower = stateOrKind.toLowerCase();
    for (const item of config.items) {
      if (
        item.id === lower ||
        item.tags.some((t) => t.toLowerCase() === lower)
      ) {
        emotionId = item.id;
        break;
      }
    }
  }

  if (!emotionId) return null;

  const entry = config.items.find((e) => e.id === emotionId);
  return entry?.data_url ?? null;
}

/** Build a lookup map from emotion ID → data_url for O(1) access. */
export function buildEmotionMap(
  config: EmotionsConfig | null,
): Map<string, string> {
  const map = new Map<string, string>();
  if (!config) return map;
  for (const item of config.items) {
    map.set(item.id, item.data_url);
  }
  return map;
}

/** Clear the in-memory cache so the next `loadEmotions` re-reads from disk. */
export function clearEmotionsCache(): void {
  cache.clear();
}
