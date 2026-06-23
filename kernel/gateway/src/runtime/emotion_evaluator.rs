// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Per-agent LLM-driven emotion evaluator.
//!
//! For agents that have a valid `emotions/` directory (data.json + all
//! referenced images present), this module starts a background task that
//! periodically:
//!
//! 1. Collects recent session messages and trace data.
//! 2. Calls an LLM to pick the most appropriate emotion from the available list.
//! 3. Publishes the result as an `emotion:evaluated` event on the global bus.
//!
//! The latest emotion ID is also stored in the registry so the SSE snapshot
//! can include it in `agent_states:updated`.
//!
//! # Gating
//!
//! If the emotions directory is missing, empty, or has invalid/missing images,
//! the evaluator is **not started** for that agent — the desktop falls back to
//! the state-based emoji mapping.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cognitive_llm::simple::parse_json_response;
use event_bus::EventBus;
use kernel::agent::AgentSystemState;
use kernel::event::{Event, EventType};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;
use tracing;

use super::session_store::SessionStore;

// ── Constants ───────────────────────────────────────────────────────────

/// Jitter range added to the interval to avoid thundering herd (±15%).
const JITTER_PCT: f64 = 0.15;

/// How many recent trace records to include as context (fixed, not configurable).
const MAX_CONTEXT_TRACES: usize = 5;

/// LLM call timeout.
const LLM_TIMEOUT_SECS: u64 = 15;

/// Max retries for transient failures (empty responses, truncations).
const MAX_RETRIES: usize = 2;

// ── Types ───────────────────────────────────────────────────────────────

/// The JSON response we expect from the LLM.
#[derive(Debug, Clone, Deserialize)]
struct EmotionResponse {
    emotion_id: String,
    #[allow(dead_code)]
    reasoning: String,
}

/// Metadata for a single available emotion (read from data.json).
#[derive(Debug, Clone)]
struct EmotionCandidate {
    id: String,
    description: String,
}

/// LLM API configuration for the emotion evaluator.
#[derive(Debug, Clone)]
pub struct EmotionLlmConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
}

/// Runtime configuration for the emotion evaluator (from config.yaml).
#[derive(Debug, Clone)]
pub struct EmotionEvalConfig {
    pub interval_secs: u64,
    pub temperature: f64,
    pub max_context_messages: usize,
    /// Max output tokens, sourced from models.<model>.max_output_tokens.
    pub max_tokens: u64,
}

// ── Evaluator ───────────────────────────────────────────────────────────

/// Per-agent background emotion evaluator.
///
/// Created per enabled agent that has a valid emotions directory.
/// Runs a loop: sleep → collect context → call LLM → publish result.
pub struct EmotionEvaluator {
    agent_id: String,
    #[allow(dead_code)]
    emotions_dir: PathBuf,
    emotion_candidates: Vec<EmotionCandidate>,
    session_store: Option<Arc<SessionStore>>,
    trace_store: Option<Arc<dyn kernel::trace::TraceStore>>,
    llm_config: EmotionLlmConfig,
    eval_config: EmotionEvalConfig,
    bus: Arc<dyn EventBus>,
    system_state: Arc<std::sync::Mutex<AgentSystemState>>,
    cancel: tokio_util::sync::CancellationToken,
    /// Shared handle for storing the latest emotion ID (read by SSE).
    latest_emotion: Arc<Mutex<Option<String>>>,
}

impl EmotionEvaluator {
    /// Create a new evaluator. Returns `None` if the emotions directory
    /// doesn't exist or doesn't pass validation.
    pub fn new(
        agent_id: String,
        session_store: Option<Arc<SessionStore>>,
        trace_store: Option<Arc<dyn kernel::trace::TraceStore>>,
        llm_config: EmotionLlmConfig,
        eval_config: EmotionEvalConfig,
        bus: Arc<dyn EventBus>,
        system_state: Arc<std::sync::Mutex<AgentSystemState>>,
    ) -> Option<Self> {
        let emotions_dir = emotions_path(&agent_id);
        let candidates = match load_emotion_candidates(&emotions_dir) {
            Some(c) => c,
            None => {
                tracing::info!(
                    agent = %agent_id,
                    "emotion evaluator skipped: no valid emotions directory"
                );
                return None;
            }
        };

        tracing::info!(
            agent = %agent_id,
            candidates = candidates.len(),
            interval_secs = eval_config.interval_secs,
            temperature = eval_config.temperature,
            "emotion evaluator created"
        );

        Some(Self {
            agent_id,
            emotions_dir,
            emotion_candidates: candidates,
            session_store,
            trace_store,
            llm_config,
            eval_config,
            bus,
            system_state,
            cancel: tokio_util::sync::CancellationToken::new(),
            latest_emotion: Arc::new(Mutex::new(None)),
        })
    }

    /// Return a shared handle to the latest emotion ID for SSE snapshots.
    pub fn latest_emotion_handle(&self) -> Arc<Mutex<Option<String>>> {
        Arc::clone(&self.latest_emotion)
    }

    /// Start the background evaluation loop.
    pub fn start(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.run_loop().await;
        });
    }

    /// Signal the evaluator to stop.
    pub fn stop(&self) {
        self.cancel.cancel();
    }

    /// Main loop: sleep → collect → evaluate → publish → repeat.
    async fn run_loop(&self) {
        // Small initial delay so the agent has time to produce some activity.
        tokio::time::sleep(Duration::from_secs(10)).await;

        loop {
            if self.cancel.is_cancelled() {
                tracing::info!(agent = %self.agent_id, "emotion evaluator stopped");
                return;
            }

            // Evaluate, racing the cancel token so a shutdown signal
            // during a long LLM call (10-30s with retries) does not have
            // to wait for the call to return. `biased;` makes the cancel
            // branch preferred when both are ready so we don't sit on a
            // completed evaluation if the operator has already asked to
            // stop. The bus publish after the select is fast enough
            // (single queue push) that wrapping it is not worth the
            // added complexity.
            let evaluation: Result<Option<EmotionResponse>, String> = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => {
                    tracing::info!(agent = %self.agent_id, "emotion evaluator stopped");
                    return;
                }
                r = self.evaluate() => r,
            };

            match evaluation {
                Ok(Some(result)) => {
                    // Store for SSE
                    {
                        let mut guard = self.latest_emotion.lock().await;
                        *guard = Some(result.emotion_id.clone());
                    }

                    // Publish to global bus
                    let _ = self
                        .bus
                        .publish(Event::new(
                            "agent:emotion",
                            EventType::Custom("emotion:evaluated".to_owned()),
                            json!({
                                "agent_id": self.agent_id,
                                "emotion_id": result.emotion_id,
                                "reasoning": result.reasoning,
                                "timestamp_ms": chrono_now_ms(),
                            }),
                        ))
                        .await;

                    tracing::debug!(
                        agent = %self.agent_id,
                        emotion = %result.emotion_id,
                        "emotion evaluated"
                    );
                }
                Ok(None) => {
                    tracing::debug!(agent = %self.agent_id, "emotion evaluation skipped (no context)");
                }
                Err(e) => {
                    tracing::warn!(agent = %self.agent_id, error = %e, "emotion evaluation failed");
                }
            }

            // Sleep with jitter
            let interval_secs = self.eval_config.interval_secs as f64;
            let jitter = (interval_secs * JITTER_PCT * (rand_simple() - 0.5) * 2.0) as u64;
            let sleep_dur = Duration::from_secs(
                (self.eval_config.interval_secs).saturating_add(jitter as i64 as u64),
            );

            tokio::select! {
                _ = self.cancel.cancelled() => {
                    tracing::info!(agent = %self.agent_id, "emotion evaluator stopped");
                    return;
                }
                _ = tokio::time::sleep(sleep_dur) => {}
            }
        }
    }

    /// Collect context, call the LLM, return the selected emotion.
    async fn evaluate(&self) -> Result<Option<EmotionResponse>, String> {
        // ── 1. Collect recent context ──────────────────────────────────
        let context = self.collect_context().await;
        if context.is_empty() {
            return Ok(None);
        }

        // ── 2. Build the prompt ────────────────────────────────────────
        let system_prompt = build_system_prompt(&self.agent_id, &self.emotion_candidates);
        let user_prompt = build_user_prompt(&context, &self.emotion_candidates);

        // ── 3. Build json_schema from emotion candidates ──────────────
        let emotion_ids: Vec<&str> = self
            .emotion_candidates
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        let response_format = serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "emotion_selection",
                "strict": true,
                "schema": {
                    "type": "object",
                    "properties": {
                        "emotion_id": {
                            "type": "string",
                            "enum": emotion_ids,
                            "description": "The selected emotion ID"
                        },
                        "reasoning": {
                            "type": "string",
                            "description": "Brief reasoning for the selection (under 60 chars)"
                        }
                    },
                    "required": ["emotion_id", "reasoning"],
                    "additionalProperties": false
                }
            }
        });

        // ── 4. Call the LLM (with retries for transient failures) ─────
        let mut last_err = String::new();
        for attempt in 0..=MAX_RETRIES {
            match self.try_evaluate(&system_prompt, &user_prompt, &response_format).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    last_err = e;
                    if attempt < MAX_RETRIES {
                        tracing::debug!(
                            agent = %self.agent_id,
                            attempt = attempt + 1,
                            error = %last_err,
                            "emotion evaluation retrying"
                        );
                        // Small backoff before retry
                        tokio::time::sleep(Duration::from_millis(500 * (attempt + 1) as u64)).await;
                    }
                }
            }
        }
        Err(last_err)
    }

    /// Single attempt: call LLM → parse → validate.
    async fn try_evaluate(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        response_format: &serde_json::Value,
    ) -> Result<Option<EmotionResponse>, String> {
        // ── Call the LLM ────────────────────────────────────────────────
        let raw = self
            .call_llm(system_prompt, user_prompt, response_format)
            .await
            .map_err(|e| format!("LLM call failed: {e}"))?;

        // ── Parse & validate (robust JSON extraction) ───────────────────
        let parsed: EmotionResponse = parse_json_response(&raw).map_err(|e| {
            format!("emotion JSON parse error: {e} — raw: {}", truncate(&raw, 200))
        })?;

        // Validate the emotion_id exists in our candidates
        if !self.emotion_candidates.iter().any(|c| c.id == parsed.emotion_id) {
            tracing::warn!(
                agent = %self.agent_id,
                emotion_id = %parsed.emotion_id,
                "LLM returned unknown emotion_id, ignoring"
            );
            return Ok(None);
        }

        Ok(Some(parsed))
    }

    /// Collect recent activity context for the LLM.
    async fn collect_context(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        // Current system state
        if let Ok(ss) = self.system_state.lock() {
            parts.push(format!(
                "Current system state: {:?}",
                ss
            ));
        }

        // Recent session messages
        if let Some(ref store) = self.session_store
            && let Ok(sessions) = store.list_all() {
                // Take the most recent session(s) with messages
                let active: Vec<_> = sessions
                    .iter()
                    .filter(|s| s.message_count > 0)
                    .take(2)
                    .collect();

                for session in active {
                    let events = store
                        .load_recent_events(&session.id, self.eval_config.max_context_messages)
                        .await;

                    if !events.is_empty() {
                        let summary: Vec<String> = events
                            .iter()
                            .map(|ev| {
                                let et = ev["event_type"].as_str().unwrap_or("?");
                                let source = ev["source"].as_str().unwrap_or("");
                                // Extract brief content from payload if available
                                let brief = ev["payload"]["text"]
                                    .as_str()
                                    .or_else(|| ev["payload"]["output"].as_str())
                                    .unwrap_or("");
                                let brief = truncate(brief, 80);
                                format!(
                                    "[{}] {}: {}",
                                    timestamp_ago(ev["timestamp_ms"].as_i64()),
                                    et,
                                    if brief.is_empty() { source } else { &brief }
                                )
                            })
                            .collect();
                        parts.push(format!(
                            "Recent session messages ({} events):\n{}",
                            summary.len(),
                            summary.join("\n")
                        ));
                    }
                }
            }

        // Recent trace records
        if let Some(ref ts) = self.trace_store {
            match ts.load_recent(&self.agent_id, MAX_CONTEXT_TRACES).await {
                Ok(traces) => {
                    if !traces.is_empty() {
                        let summary: Vec<String> = traces
                            .iter()
                            .map(|tr| {
                                format!(
                                    "- {} ({}ms, outcome: {:?}, tools: {})",
                                    truncate(&tr.description, 60),
                                    tr.duration_ms,
                                    tr.outcome,
                                    tr.tool_calls.len(),
                                )
                            })
                            .collect();
                        parts.push(format!(
                            "Recent traces:\n{}",
                            summary.join("\n")
                        ));
                    }
                }
                Err(e) => {
                    tracing::debug!(agent = %self.agent_id, error = %e, "trace store read skipped");
                }
            }
        }

        parts.join("\n\n")
    }

    /// Call the LLM and return the raw text response.
    async fn call_llm(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        response_format: &serde_json::Value,
    ) -> Result<String, String> {
        let url = format!(
            "{}/chat/completions",
            self.llm_config.base_url.trim_end_matches('/')
        );

        let mut body = json!({
            "model": self.llm_config.model,
            "temperature": self.eval_config.temperature,
            "max_tokens": self.eval_config.max_tokens,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": user_prompt },
            ],
        });
        if let Some(obj) = body.as_object_mut() {
            obj.insert("response_format".into(), response_format.clone());
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(LLM_TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("http client: {e}"))?;

        let mut req = client.post(&url).json(&body);

        if let Some(ref key) = self.llm_config.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }

        let resp = req.send().await.map_err(|e| format!("http: {e}"))?;

        let status = resp.status();
        let raw_body = resp.text().await.map_err(|e| format!("body: {e}"))?;

        if !status.is_success() {
            return Err(format!("LLM API {status}: {}", truncate(&raw_body, 300)));
        }

        // Parse OpenAI-compatible response
        let v: serde_json::Value =
            serde_json::from_str(&raw_body).map_err(|e| format!("json: {e}"))?;

        // Log finish_reason for diagnostics (truncation vs refusal vs stop)
        let finish_reason = v["choices"][0]["finish_reason"]
            .as_str()
            .unwrap_or("unknown");

        let content = v["choices"][0]["message"]["content"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                format!(
                    "empty content from LLM (finish_reason={finish_reason}, body={})",
                    truncate(&raw_body, 200)
                )
            })?;

        if finish_reason == "length" {
            tracing::warn!(
                agent = %self.agent_id,
                "LLM response truncated (finish_reason=length) — consider raising max_tokens"
            );
        }

        // Robust JSON extraction: handle markdown fences
        Ok(extract_json(content))
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Build the path to an agent's emotions directory.
fn emotions_path(agent_id: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".aman")
        .join("agents")
        .join(agent_id)
        .join("emotions")
}

/// Load and validate the emotion candidates from `data.json`.
/// Returns `None` if the directory is missing, data.json can't be parsed,
/// or any referenced image file is missing.
fn load_emotion_candidates(dir: &Path) -> Option<Vec<EmotionCandidate>> {
    if !dir.exists() {
        return None;
    }

    let data_path = dir.join("data.json");
    let raw = std::fs::read_to_string(&data_path).ok()?;

    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;

    let img_ext = parsed["img_ext"].as_str().unwrap_or("png");

    let items = parsed["items"].as_array()?;
    if items.is_empty() {
        return None;
    }

    let mut candidates = Vec::with_capacity(items.len());

    for item in items {
        let id = item["id"].as_str()?.to_owned();
        let description = item["description"].as_str().unwrap_or("").to_owned();

        // Validate the image file exists
        let img_path = dir.join(format!("{}.{}", id, img_ext));
        if !img_path.exists() {
            tracing::warn!("emotion image missing: {}", img_path.display());
            return None;
        }

        candidates.push(EmotionCandidate { id, description });
    }

    Some(candidates)
}

/// Build the system prompt for the LLM.
fn build_system_prompt(agent_id: &str, candidates: &[EmotionCandidate]) -> String {
    let emotion_list: Vec<String> = candidates
        .iter()
        .map(|c| format!("- \"{}\": {}", c.id, c.description))
        .collect();

    format!(
        "You are evaluating the emotional state of an AI agent named \"{agent_id}\".\n\
         Given the agent's recent activity context, select the SINGLE most appropriate \
         emotion from the list below.\n\n\
         Available emotions:\n{emotions}\n\n\
         Respond with valid JSON only, in this exact format:\n\
         {{\"emotion_id\": \"<id>\", \"reasoning\": \"<under 60 chars>\"}}\n\n\
         CRITICAL: Keep reasoning under 60 characters. Return ONLY the JSON object, \
         no markdown fences, no extra text.",
        agent_id = agent_id,
        emotions = emotion_list.join("\n"),
    )
}

/// Build the user prompt with the collected agent context.
fn build_user_prompt(context: &str, candidates: &[EmotionCandidate]) -> String {
    let ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
    format!(
        "Recent agent activity:\n\n{context}\n\n\
         Pick the best emotion ID from: {ids}",
        context = context,
        ids = ids.join(", "),
    )
}

/// Extract JSON from LLM output, handling markdown code fences.
fn extract_json(raw: &str) -> String {
    let trimmed = raw.trim();

    // Try to extract from ```json ... ``` fence
    if let Some(start) = trimmed.find("```json") {
        let after = &trimmed[start + 7..];
        if let Some(end) = after.find("```") {
            return after[..end].trim().to_owned();
        }
    }

    // Try to extract from ``` ... ``` fence (no language tag)
    if let Some(start) = trimmed.find("```") {
        let after = &trimmed[start + 3..];
        if let Some(end) = after.find("```") {
            return after[..end].trim().to_owned();
        }
    }

    // Return as-is (assume it's bare JSON)
    trimmed.to_owned()
}

/// Truncate a string to `max` chars, appending "…" if truncated.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// Simple timestamp-based "how long ago" for context.
fn timestamp_ago(ts_ms: Option<i64>) -> String {
    let Some(ms) = ts_ms else { return "?".to_owned() };
    let now_ms = chrono_now_ms();
    let delta_secs = (now_ms - ms) / 1000;
    if delta_secs < 60 {
        format!("{}s ago", delta_secs)
    } else if delta_secs < 3600 {
        format!("{}m ago", delta_secs / 60)
    } else {
        format!("{}h ago", delta_secs / 3600)
    }
}

/// Current time in milliseconds since epoch (fractional, as f64 for
/// rough relative-time display; no need for precise wall-clock).
///
/// Uses `std::time::SystemTime` — this is fine for relative display;
/// it is NOT `Date.now()`-style randomness and WILL be stable across
/// workflow resumes.
fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Simple pseudo-random value in [0.0, 1.0) for jitter.
/// Uses a naive LCG — sufficient for jitter, not crypto.
fn rand_simple() -> f64 {
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (ns as f64 % 10_000.0) / 10_000.0
}
