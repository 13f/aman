// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Reflection runner — triggered by QueueDrained events when the event bus
//! transitions from busy to empty (or on cold start after a 3–5s grace timer).
//! Extracts structured summaries from one unreflected session per cycle via
//! LLM and stores them in the memory provider for long-term retention.
//!
//! Reflection is one of the eight idle states but is NOT driven by idle depth.
//! It is triggered by the Dispatcher (or AgentIdleManager cold-start) via
//! `system.queue_drained` events, and executed with `tokio::select!` so new
//! events can preempt it. See docs/idle-design.md §4.1.

use async_trait::async_trait;
use config::MemoryLlmConfig;
use event_bus::EventHandler;
use kernel::event::{Event, EventType};
use kernel::react::ChatMessage;
use cognitive_llm::simple::parse_json_response;
use kernel::llm::{LlmChatRequest, LlmProvider, ResponseFormat};
use kernel::memory::MemoryProvider;
use kernel::trace::{TraceOutcome, TraceRecord};
use kernel::AmanResult;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Classify a chat-completion error for log-tiered reporting.
///
/// Provider implementations map transport problems (timeout, EOF, reset,
/// DNS failure, 5xx) onto `kernel::Error::Io` / `Timeout`, which we treat as
/// *transient* — the same session is left un-reflected and will be retried
/// automatically on the next QueueDrained cycle. Everything else (auth,
/// schema-rejection, model-not-found) is presented to the operator.
#[must_use]
fn is_transient_llm_error(err: &kernel::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("timed out")
        || msg.contains("timeout")
        || msg.contains("connection")
        || msg.contains("reset")
        || msg.contains("dns")
        || msg.contains("refused")
        || msg.contains("eof")
        || msg.contains("streaming error http 5")
}

use super::agent_registry::AgentRegistry;

/// Resolved LLM backend for reflection — pairs a provider with the model name
/// that should be sent to it.
///
/// The model travels with the provider because the two must be consistent:
/// a DeepSeek model name is meaningless at LongCat's endpoint and vice versa.
/// When the dedicated `memory.llm` provider is unavailable and we fall back to
/// the agent's own provider, the model must switch to the agent's configured
/// model too — otherwise the wrong model name hits the wrong endpoint (the bug
/// this struct exists to fix).
#[derive(Clone)]
struct ReflectionLlm {
    provider: Arc<dyn kernel::llm::LlmProvider>,
    model: String,
}

/// Handles QueueDrained → reflection for all agents.
///
/// Subscribes to the global event bus and processes
/// [`EventType::QueueDrained`](kernel::event::EventType::QueueDrained) events
/// from all agents. Dependencies are injected via the `OnceLock` pattern
/// (same as `ReadSkillTool`).
///
/// QueueDrained events are produced in two cases:
/// 1. **Busy→empty transition**: AgentIdleManager detects the bus was busy then
///    became empty, meaning the Dispatcher just finished processing events.
/// 2. **Cold start**: Agent starts with an empty queue — after a 3–5s grace
///    timer, a synthetic QueueDrained is produced so Reflection runs at least
///    once before entering the idle depth sequence.
pub struct ReflectionRunner {
    agent_registry: OnceLock<Arc<AgentRegistry>>,
    memory_llm: OnceLock<MemoryLlmConfig>,
    /// Dedicated LLM provider for memory/extraction work, built at startup
    /// from `memory.llm.provider` + `memory.llm.model`. Stored separately so
    /// reflection can use a different backend than the agent's default.
    memory_llm_provider: OnceLock<Arc<dyn kernel::llm::LlmProvider>>,
}

impl ReflectionRunner {
    pub fn new() -> Self {
        Self {
            agent_registry: OnceLock::new(),
            memory_llm: OnceLock::new(),
            memory_llm_provider: OnceLock::new(),
        }
    }

    pub fn set_agent_registry(&self, registry: Arc<AgentRegistry>) {
        let _ = self.agent_registry.set(registry);
    }

    pub fn set_memory_llm(&self, config: MemoryLlmConfig) {
        let _ = self.memory_llm.set(config);
    }

    /// Set the dedicated LLM provider for memory/extraction work. Called by
    /// `AgentRuntime` after it has built the provider from the config's
    /// `providers:` map. This is the single source of truth for reflection's
    /// LLM backend — it always wins over the agent's default provider.
    pub fn set_memory_llm_provider(&self, provider: Arc<dyn kernel::llm::LlmProvider>) {
        let _ = self.memory_llm_provider.set(provider);
    }

    /// Resolve the LLM provider best suited for reflection's extraction call.
    ///
    /// Priority:
    /// 1. Use the dedicated memory LLM provider if `AgentRuntime` wired one
    ///    (built from `memory.llm.*` at startup). This is the operator's
    ///    explicit choice — reflection always wins over the agent's default.
    /// 2. Otherwise, fall back to the agent's default provider (legacy).
    /// 3. If neither is available, return `None`.
    async fn resolve_reflection_llm(
        &self,
        registry: &super::agent_registry::AgentRegistry,
        agent_id: &str,
    ) -> Option<ReflectionLlm> {
        if let Some(dedicated) = self.memory_llm_provider.get() {
            // Dedicated provider: pair it with the dedicated model from
            // `memory.llm.model` (or "default" if the operator left it unset).
            let model = self
                .memory_llm
                .get()
                .map(|c| c.model.clone())
                .unwrap_or_else(|| "default".to_owned());
            debug!(
                agent_id,
                model = %model,
                "Reflection: using dedicated memory.llm provider"
            );
            return Some(ReflectionLlm {
                provider: Arc::clone(dedicated),
                model,
            });
        }
        // Fallback: legacy agent provider. Crucially, pair it with the agent's
        // own configured model — NOT memory.llm.model. Sending a DeepSeek model
        // name to LongCat's endpoint (or vice versa) is what produced the
        // "Unsupported model" 400 error.
        if let Some(instance) = registry.get(agent_id).await {
            let model = instance.descriptor.model.clone();
            if let Some(provider) = registry.get_llm_provider(agent_id).await {
                debug!(
                    agent_id,
                    model = %model,
                    provider = %instance.descriptor.provider,
                    "Reflection: fallback to agent provider with agent model"
                );
                return Some(ReflectionLlm { provider, model });
            }
        }
        debug!(agent_id, "Reflection: no LLM resolvable for agent");
        None
    }

    // -- session_extract ------------------------------------------------------

    /// Maximum number of recent events to load for extraction.
    /// Avoids sending multi-megabyte conversations to the LLM.
    const MAX_EXTRACTION_EVENTS: usize = 200;
    /// Hard cap on formatted conversation size (chars) sent to LLM.
    const MAX_CONVERSATION_CHARS: usize = 48000;

    /// Query one unreflected session, extract structured summary via LLM, and
    /// store in the per-agent memory provider. Mark the session as reflected
    /// on success. Processes at most one session per invocation.
    async fn session_extract(&self, agent_id: &str) {
        let Some(registry) = self.agent_registry.get() else {
            info!("Reflection: no AgentRegistry, skipping session_extract");
            return;
        };

        // Service degradation: skip LLM calls when backend is Down.
        if let Some(health) = registry.get_agent_backend_health(agent_id).await {
            if health.status() == super::backend_health::BackendStatus::Down {
                debug!(agent_id, "Reflection: LLM backend down, skipping session_extract");
                return;
            }
        }

        let Some(store) = registry.get_session_store(agent_id).await else {
            debug!(agent_id, "Reflection: no SessionStore for agent, skipping");
            return;
        };
        let Some(memory) = registry.get_memory_provider(agent_id).await else {
            debug!(agent_id, "Reflection: no MemoryProvider for agent, skipping");
            return;
        };
        // Resolve the LLM provider for reflection. Three-step fallback:
        //   1. If memory.llm.provider is configured, use that dedicated provider
        //      (so reflection uses the exact backend the operator selected).
        //   2. Otherwise, fall back to the agent's default provider.
        //   3. If neither is available, skip this cycle.
        let ReflectionLlm { provider: llm, model } =
            match self.resolve_reflection_llm(registry, agent_id).await {
                Some(resolved) => resolved,
                None => {
                    debug!(agent_id, "Reflection: no LLM resolvable for agent, skipping");
                    return;
                }
            };

        let session = match store.list_unreflected() {
            Ok(Some(s)) => s,
            Ok(None) => {
                debug!("Reflection: no unreflected sessions");
                return;
            }
            Err(e) => {
                warn!(error = %e, "Reflection: failed to list unreflected sessions");
                return;
            }
        };

        // Load only recent events — full history is too large for LLM context
        let events = store.load_recent_events(&session.id, Self::MAX_EXTRACTION_EVENTS).await;
        if events.len() < 2 {
            // Mark as reflected even if not enough content — don't retry forever
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            if let Err(e) = store.mark_reflected(&session.id, now) {
                tracing::warn!(session_id = %session.id, error = %e, "failed to mark session as reflected; may be re-processed");
            }
            return;
        }

        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        info!(
            agent_id,
            session_id = %session.id,
            event_count = events.len(),
            "Reflection: extracting session",
        );

        match self.extract_and_store(&llm, &model, &memory, agent_id, &session.id, &events, Self::MAX_CONVERSATION_CHARS).await {
            Ok(()) => {
                // Report success to BackendHealth.
                if let Some(health) = registry.get_agent_backend_health(agent_id).await {
                    let changed = health.record_success(
                        registry.backend_health_registry().config(),
                    );
                    if let Some(ev) = changed {
                        publish_health_event(&registry, ev);
                    }
                }
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                if let Err(e) = store.mark_reflected(&session.id, now) {
                tracing::warn!(session_id = %session.id, error = %e, "failed to mark session as reflected; may be re-processed");
            }
                info!(
                    agent_id,
                    session_id = %session.id,
                    "Reflection: session extracted to memory"
                );

                // Write a trace record for downstream consumers (Meditation, etc.)
                if let Some(ts) = registry.get_trace_store(agent_id).await {
                    let entities = extract_entities_from_events(&events);
                    let trace = TraceRecord {
                        trace_id: format!("refl_{}", Uuid::now_v7()),
                        agent_id: agent_id.to_owned(),
                        session_id: Some(session.id.clone()),
                        task_type: "session_extract".to_owned(),
                        description: format!(
                            "Reflected session {} with {} events",
                            &session.id[..16.min(session.id.len())],
                            events.len()
                        ),
                        input: String::new(),
                        outcome: TraceOutcome::Success,
                        duration_ms: (now - started_at) as u64,
                        decision_points: Vec::new(),
                        tool_calls: Vec::new(),
                        errors: Vec::new(),
                        entities,
                        started_at_ms: started_at,
                        ended_at_ms: Some(now),
                    };
                    if let Err(e) = ts.save_trace(&trace).await {
                        debug!(agent_id, error = %e, "Reflection: failed to save extraction trace");
                    }
                }
            }
            Err(e) => {
                // Report failure to BackendHealth for cognitive state tracking.
                if let Some(registry) = self.agent_registry.get() {
                    if let Some(health) = registry.get_agent_backend_health(agent_id).await {
                        let changed = health.record_failure(
                            &e.to_string(),
                            registry.backend_health_registry().config(),
                        );
                        if let Some(ev) = changed {
                            publish_health_event(registry, ev);
                        }
                    }
                }
                // Transient provider blips (timeout / EOF / 5xx) don't mark
                // the session, so it's retried on the next cycle — log at
                // info to keep `warn` reserved for real operator issues.
                if is_transient_llm_error(&e) {
                    info!(
                        agent_id,
                        session_id = %session.id,
                        error = %e,
                        "Reflection: LLM extraction call transient — \
                         timeout/connection/5xx — will retry next QueueDrained"
                    );
                } else {
                    warn!(
                        agent_id,
                        session_id = %session.id,
                        error = %e,
                        "Reflection: LLM extraction call failed for agent {agent_id}"
                    );
                }
            }
        }
    }

    // -- TraceStore-backed reflection steps (idle-patch.md §7 steps 1-3) ------

    /// Step 1: Detect incomplete task chains via TraceStore.
    ///
    /// Queries `find_incomplete` for partial traces with no `ended_at_ms`,
    /// categorizes them by `task_type` via `load_by_task_type`, logs them
    /// and publishes a low-priority event so the UI / operator can inspect
    /// stalled task chains.
    async fn chain_tasks(&self, agent_id: &str) {
        let Some(registry) = self.agent_registry.get() else {
            return;
        };
        let Some(ts) = registry.get_trace_store(agent_id).await else {
            debug!(agent_id, "Reflection::chain_tasks: no TraceStore");
            return;
        };

        match ts.find_incomplete(agent_id).await {
            Ok(incomplete) if incomplete.is_empty() => {
                debug!(agent_id, "Reflection::chain_tasks: no incomplete traces");
            }
            Ok(incomplete) => {
                info!(
                    agent_id,
                    count = incomplete.len(),
                    "Reflection::chain_tasks: incomplete traces detected",
                );

                // Categorize incomplete traces by task_type for targeted
                // recovery analysis (e.g. all stalled "skill_run" tasks may
                // indicate a tool timeout).
                let mut task_types: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for trace in &incomplete {
                    *task_types
                        .entry(trace.task_type.clone())
                        .or_default() += 1;
                    debug!(
                        agent_id,
                        trace_id = %trace.trace_id,
                        task_type = %trace.task_type,
                        description = %trace.description,
                        "Reflection::chain_tasks: stalled task",
                    );
                }
                if task_types.len() > 1 {
                    info!(
                        agent_id,
                        ?task_types,
                        "Reflection::chain_tasks: stalled tasks by type",
                    );
                }

                // Use load_by_task_type to check if any specific task_type
                // has a history of failures that may explain the stalls.
                for (task_type, count) in &task_types {
                    if *count >= 2 {
                        match ts
                            .load_by_task_type(agent_id, task_type, 5)
                            .await
                        {
                            Ok(recent) => {
                                let failures = recent
                                    .iter()
                                    .filter(|t| {
                                        matches!(
                                            t.outcome,
                                            kernel::trace::TraceOutcome::Failure
                                        )
                                    })
                                    .count();
                                if failures > 0 {
                                    info!(
                                        agent_id,
                                        task_type,
                                        failures,
                                        recent_count = recent.len(),
                                        "Reflection::chain_tasks: prior failures for stalled task type",
                                    );
                                }
                            }
                            Err(e) => {
                                debug!(
                                    agent_id,
                                    task_type,
                                    error = %e,
                                    "Reflection::chain_tasks: load_by_task_type failed",
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!(agent_id, error = %e, "Reflection::chain_tasks: find_incomplete failed");
            }
        }
    }

    /// Step 2: Extract and classify errors from recent traces.
    ///
    /// Loads the most recent traces, groups errors by `error_type`, and logs a
    /// summary. Unrecovered errors are flagged at warn level for operator
    /// visibility.
    async fn immediate_errors(&self, agent_id: &str) {
        let Some(registry) = self.agent_registry.get() else {
            return;
        };
        let Some(ts) = registry.get_trace_store(agent_id).await else {
            debug!(agent_id, "Reflection::immediate_errors: no TraceStore");
            return;
        };

        let traces = match ts.load_recent(agent_id, 30).await {
            Ok(t) => t,
            Err(e) => {
                warn!(agent_id, error = %e, "Reflection::immediate_errors: load_recent failed");
                return;
            }
        };

        if traces.is_empty() {
            return;
        }

        // Group errors by type: (error_type, count, total_recovered, messages)
        let mut error_groups: HashMap<String, (usize, usize, Vec<String>)> = HashMap::new();
        for trace in &traces {
            for err in &trace.errors {
                let entry = error_groups
                    .entry(err.error_type.clone())
                    .or_insert((0, 0, Vec::new()));
                entry.0 += 1;
                if err.recovered {
                    entry.1 += 1;
                }
                if entry.2.len() < 3 {
                    entry.2.push(err.error_message.clone());
                }
            }
        }

        if !error_groups.is_empty() {
            info!(
                agent_id,
                error_categories = error_groups.len(),
                traces_scanned = traces.len(),
                "Reflection::immediate_errors: error classification complete",
            );
            for (error_type, (count, recovered, messages)) in &error_groups {
                let unrecovered = count - recovered;
                if unrecovered > 0 {
                    warn!(
                        agent_id,
                        error_type,
                        count,
                        recovered,
                        unrecovered,
                        samples = ?messages,
                        "Reflection::immediate_errors: unrecovered errors",
                    );
                } else {
                    debug!(
                        agent_id,
                        error_type,
                        count,
                        recovered,
                        "Reflection::immediate_errors: all recovered",
                    );
                }
            }
        }
    }

    /// Step 3: Extract lessons learned from trace outcomes.
    ///
    /// Scans recent traces for successful recovery paths and decision patterns,
    /// then stores them as procedural memory for future task guidance.
    async fn lessons_learned(&self, agent_id: &str) {
        let Some(registry) = self.agent_registry.get() else {
            return;
        };
        let Some(ts) = registry.get_trace_store(agent_id).await else {
            debug!(agent_id, "Reflection::lessons_learned: no TraceStore");
            return;
        };
        let Some(memory) = registry.get_memory_provider(agent_id).await else {
            debug!(agent_id, "Reflection::lessons_learned: no MemoryProvider");
            return;
        };

        let traces = match ts.load_recent(agent_id, 20).await {
            Ok(t) => t,
            Err(e) => {
                warn!(agent_id, error = %e, "Reflection::lessons_learned: load_recent failed");
                return;
            }
        };

        let mut lessons_stored = 0u64;
        for trace in &traces {
            // Lesson 1: Successful recovery patterns
            for err in &trace.errors {
                if err.recovered && err.recovery_action.is_some() {
                    let lesson = serde_json::json!({
                        "error_type": err.error_type,
                        "recovery_action": err.recovery_action,
                        "trace_id": trace.trace_id,
                        "task_type": trace.task_type,
                    });
                    let _ = memory.store(
                        agent_id,
                        &lesson.to_string(),
                        vec![
                            "lesson_learned".into(),
                            "recovery".into(),
                            err.error_type.clone(),
                        ],
                    );
                    lessons_stored += 1;
                }
            }

            // Lesson 2: Decision patterns for successful outcomes
            if trace.outcome == TraceOutcome::Success && !trace.decision_points.is_empty() {
                for dp in &trace.decision_points {
                    if !dp.alternatives.is_empty() {
                        let lesson = serde_json::json!({
                            "branch": dp.branch,
                            "taken": dp.taken,
                            "alternatives": dp.alternatives,
                            "trace_id": trace.trace_id,
                            "task_type": trace.task_type,
                        });
                        let _ = memory.store(
                            agent_id,
                            &lesson.to_string(),
                            vec![
                                "lesson_learned".into(),
                                "decision".into(),
                                "success".into(),
                            ],
                        );
                        lessons_stored += 1;
                    }
                }
            }
        }

        if lessons_stored > 0 {
            info!(
                agent_id,
                count = lessons_stored,
                traces_scanned = traces.len(),
                "Reflection::lessons_learned: stored",
            );
        } else {
            debug!(agent_id, "Reflection::lessons_learned: no new lessons");
        }
    }

    async fn extract_and_store(
        &self,
        llm: &Arc<dyn LlmProvider>,
        model: &str,
        memory: &Arc<dyn MemoryProvider>,
        agent_id: &str,
        session_id: &str,
        events: &[serde_json::Value],
        max_chars: usize,
    ) -> AmanResult<()> {
        session_extract_and_store(
            llm,
            model,
            memory,
            agent_id,
            session_id,
            events,
            max_chars,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Shared extraction helpers — used by both ReflectionRunner (QueueDrained)
// and SleepRunner Phase 1 (backfill for sessions Reflection missed).
// ---------------------------------------------------------------------------

/// Run LLM extraction on one session's events and store the structured
/// summary in the per-agent memory provider. Also creates KG relationships
/// for extracted entities.
///
/// `model` is the API-level model name to send with the request. It must be
/// consistent with `llm` (e.g. a LongCat model name for a LongCat provider).
/// Callers are responsible for pairing the right model with the right provider
/// — see [`resolve_reflection_llm`](ReflectionRunner::resolve_reflection_llm).
pub async fn session_extract_and_store(
    llm: &Arc<dyn LlmProvider>,
    model: &str,
    memory: &Arc<dyn MemoryProvider>,
    agent_id: &str,
    session_id: &str,
    events: &[serde_json::Value],
    max_chars: usize,
) -> AmanResult<()> {
    session_extract_and_store_with_prompt(
        llm, model, memory, agent_id, session_id, events, max_chars, None,
    )
    .await
}

/// Run LLM extraction with an optional prompt override from the Python self-module bridge.
///
/// `model` is the API-level model name sent with the request. It is the
/// caller's responsibility to pass a model name consistent with `llm` — the
/// helper no longer reads `memory.llm.model`, since that value may belong to a
/// different provider than the one actually being used (fallback path).
#[allow(clippy::too_many_arguments)] // Mirrors the internal extraction pipeline inputs.
pub async fn session_extract_and_store_with_prompt(
    llm: &Arc<dyn LlmProvider>,
    model: &str,
    memory: &Arc<dyn MemoryProvider>,
    agent_id: &str,
    session_id: &str,
    events: &[serde_json::Value],
    max_chars: usize,
    extraction_prompt_override: Option<String>,
) -> AmanResult<()> {
    let conversation = format_conversation(events, max_chars);
    let system_prompt = extraction_prompt(extraction_prompt_override);

    let req = LlmChatRequest {
        model: model.to_owned(),
        system_prompt,
        messages: vec![ChatMessage::user(conversation)],
        tools: Vec::new(),
        max_output_tokens: 1024,
        response_format: Some(ResponseFormat::JsonSchema {
            name: "session_extraction".into(),
            strict: true,
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "intent": { "type": "string" },
                    "decisions": { "type": "array", "items": { "type": "string" } },
                    "outputs": { "type": "array", "items": { "type": "string" } },
                    "errors": { "type": "array", "items": { "type": "string" } },
                    "tags": { "type": "array", "items": { "type": "string" } },
                    "entities": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["intent", "decisions", "outputs", "errors", "tags", "entities"],
                "additionalProperties": false
            }),
        }),
    };

    let resp = llm.chat_completion(req, None).await.map_err(|e| {
        kernel::Error::Unrecoverable {
            message: format!("Reflection LLM call failed: {e}"),
        }
    })?;

    // Parse the structured JSON from LLM response (robust: handles edge cases)
    let summary: serde_json::Value =
        parse_json_response(&resp.content).unwrap_or_else(|_| {
            serde_json::json!({
                "intent": "unknown",
                "raw": resp.content,
            })
        });

    // Store the summary in memory
    let summary_json = serde_json::to_string(&summary).unwrap_or_default();
    memory.store(
        agent_id,
        &summary_json,
        vec!["session_extract".into(), session_id.to_owned()],
    );

    // Create KG relationships for extracted entities
    if let Some(entities) = summary.get("entities").and_then(|e| e.as_array()) {
        for entity in entities {
            if let Some(name) = entity.as_str()
                && let Err(e) = memory
                    .relate(name, session_id, "appears_in")
                    .await
                {
                    tracing::warn!(entity = %name, session_id = %session_id, error = %e, "failed to create KG relationship");
                }
        }
    }

    Ok(())
}

/// Format conversation events into a compact text for LLM extraction.
///
/// Each event is formatted as `[event_type] payload\n`. Payloads over
/// 2000 chars are truncated. Formatting stops once `max_chars` is reached
/// (the last incomplete line is omitted to avoid garbled output).
pub fn format_conversation(events: &[serde_json::Value], max_chars: usize) -> String {
    let mut out = String::with_capacity(max_chars.min(65536));
    for event in events {
        let event_type = event
            .get("event_type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let payload = event.get("payload").map(|p| p.to_string()).unwrap_or_default();
        // Truncate large payloads at a char boundary
        let payload = if payload.len() > 2000 {
            let trunc_byte = payload
                .char_indices()
                .nth(2000)
                .map(|(i, _)| i)
                .unwrap_or(payload.len());
            format!("{}…(truncated)", &payload[..trunc_byte])
        } else {
            payload
        };
        let line = format!("[{event_type}] {payload}\n");
        if out.len() + line.len() > max_chars {
            break;
        }
        out.push_str(&line);
    }
    out
}

/// System prompt for session extraction.
///
/// When `override_prompt` is `Some`, uses the Python self-module bridge
/// output (Phase 2+). Otherwise uses the hardcoded Rust default.
pub fn extraction_prompt(override_prompt: Option<String>) -> String {
    if let Some(prompt) = override_prompt.filter(|p| !p.is_empty()) {
        return prompt;
    }
    r#"You are a memory extraction assistant. Given a conversation log between a user and an AI agent, extract a structured JSON summary with these fields:

- "intent": the user's primary goal in one sentence
- "decisions": array of key decisions made during the conversation
- "outputs": array of concrete results or deliverables produced
- "errors": array of errors encountered and how they were resolved
- "tags": array of topic tags for categorization
- "entities": array of named entities mentioned (people, tools, projects, etc.)

Respond with ONLY valid JSON, no markdown or explanation."#
        .to_owned()
}


/// Scan event payloads for entity names suitable for trace records.
///
/// Extracts tool names, agent references, and key identifiers from event
/// payloads. Deduplicates and limits to avoid bloating trace files.
fn extract_entities_from_events(events: &[serde_json::Value]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut entities = Vec::new();
    for event in events.iter().take(200) {
        // Extract tool_name from tool call events
        if let Some(tool) = event
            .get("payload")
            .and_then(|p| p.get("tool_name"))
            .and_then(|v| v.as_str())
            && seen.insert(tool.to_owned()) {
                entities.push(tool.to_owned());
            }
        // Extract from event_type (e.g., "tool:exec:read_file")
        let event_type = event
            .get("event_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if event_type.contains(':') {
            for part in event_type.split(':') {
                if part.len() > 2 && seen.insert(part.to_owned()) {
                    entities.push(part.to_owned());
                }
            }
        }
        if entities.len() >= 30 {
            break;
        }
    }
    entities
}

#[async_trait]
impl EventHandler for ReflectionRunner {
    async fn handle(&self, event: Event) -> AmanResult<()> {
        // Only process QueueDrained events (busy→empty transition or cold start)
        if event.event_type != EventType::QueueDrained {
            return Ok(());
        }

        let agent_id = event
            .payload
            .get("agentId")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        info!(
            agent_id,
            "Reflection: QueueDrained received, running cycle"
        );

        // Step 1: Detect incomplete task chains
        self.chain_tasks(agent_id).await;

        // Step 2: Classify and report errors from recent traces
        self.immediate_errors(agent_id).await;

        // Step 3: Extract lessons learned from trace outcomes
        self.lessons_learned(agent_id).await;

        // Step 4: Extract one unreflected session per cycle
        self.session_extract(agent_id).await;

        Ok(())
    }
}

/// Publish a backend health change event to the event bus.
///
/// `Unknown → Ok`（启动后首次连通）单独标记为 `llm_backend_connected`，
/// 以便通知层显示"LLM 服务已连接"欢迎消息，而非通用"已恢复"文案。
/// 其他任何非 Ok → Ok 翻转仍沿用 `llm_backend_recovered`。
pub fn publish_health_event(registry: &AgentRegistry, changed: super::backend_health::BackendHealthChanged) {
    let event_type = match (changed.from, changed.to) {
        (super::backend_health::BackendStatus::Unknown, super::backend_health::BackendStatus::Ok) => "llm_backend_connected",
        (_, super::backend_health::BackendStatus::Ok) => "llm_backend_recovered",
        (_, super::backend_health::BackendStatus::Degraded) => "llm_backend_degraded",
        (_, super::backend_health::BackendStatus::Down) => "llm_backend_down",
        (_, super::backend_health::BackendStatus::Unknown) => "llm_backend_unknown",
    };
    let payload = match serde_json::to_value(&changed) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize BackendHealthChanged");
            serde_json::json!({ "base_url": changed.base_url })
        }
    };
    let bus = registry.bus().clone();
    tokio::spawn(async move {
        let _ = bus
            .publish(Event::new(
                "llm_health",
                EventType::Custom(event_type.to_owned()),
                payload,
            ))
            .await;
    });
}
