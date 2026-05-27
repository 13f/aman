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
use kernel::llm::{LlmChatRequest, LlmProvider};
use kernel::memory::MemoryProvider;
use kernel::trace::{TraceOutcome, TraceRecord};
use kernel::AmanResult;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::agent_registry::AgentRegistry;

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
}

impl ReflectionRunner {
    pub fn new() -> Self {
        Self {
            agent_registry: OnceLock::new(),
            memory_llm: OnceLock::new(),
        }
    }

    pub fn set_agent_registry(&self, registry: Arc<AgentRegistry>) {
        let _ = self.agent_registry.set(registry);
    }

    pub fn set_memory_llm(&self, config: MemoryLlmConfig) {
        let _ = self.memory_llm.set(config);
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
        let Some(store) = registry.get_session_store(agent_id).await else {
            debug!(agent_id, "Reflection: no SessionStore for agent, skipping");
            return;
        };
        let Some(memory) = registry.get_memory_provider(agent_id).await else {
            debug!(agent_id, "Reflection: no MemoryProvider for agent, skipping");
            return;
        };
        let Some(llm) = registry.get_llm_provider(agent_id).await else {
            debug!(agent_id, "Reflection: no LlmProvider for agent, skipping");
            return;
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
            let _ = store.mark_reflected(&session.id, now);
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

        match self.extract_and_store(&llm, &memory, agent_id, &session.id, &events, Self::MAX_CONVERSATION_CHARS).await {
            Ok(()) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                let _ = store.mark_reflected(&session.id, now);
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
                warn!(
                    agent_id,
                    session_id = %session.id,
                    error = %e,
                    "Reflection: session_extract failed"
                );
            }
        }
    }

    // -- TraceStore-backed reflection steps (idle-patch.md §7 steps 1-3) ------

    /// Step 1: Detect incomplete task chains via TraceStore.
    ///
    /// Queries `find_incomplete` for partial traces with no `ended_at_ms`, logs
    /// them and publishes a low-priority event so the UI / operator can inspect
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
                for trace in &incomplete {
                    debug!(
                        agent_id,
                        trace_id = %trace.trace_id,
                        task_type = %trace.task_type,
                        description = %trace.description,
                        "Reflection::chain_tasks: stalled task",
                    );
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
        memory: &Arc<dyn MemoryProvider>,
        agent_id: &str,
        session_id: &str,
        events: &[serde_json::Value],
        max_chars: usize,
    ) -> AmanResult<()> {
        session_extract_and_store(
            self.memory_llm.get(),
            llm,
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
pub async fn session_extract_and_store(
    memory_llm: Option<&MemoryLlmConfig>,
    llm: &Arc<dyn LlmProvider>,
    memory: &Arc<dyn MemoryProvider>,
    agent_id: &str,
    session_id: &str,
    events: &[serde_json::Value],
    max_chars: usize,
) -> AmanResult<()> {
    let conversation = format_conversation(events, max_chars);
    let system_prompt = extraction_prompt();

    let model = memory_llm
        .map(|c| c.model.as_str())
        .unwrap_or("default");

    let req = LlmChatRequest {
        model: model.to_owned(),
        system_prompt,
        messages: vec![ChatMessage::user(conversation)],
        tools: Vec::new(),
        max_output_tokens: 1024,
    };

    let resp = llm.chat_completion(req, None).await.map_err(|e| {
        kernel::Error::Unrecoverable {
            message: format!("Reflection LLM call failed: {e}"),
        }
    })?;

    // Parse the structured JSON from LLM response
    let summary: serde_json::Value =
        serde_json::from_str(&resp.content).unwrap_or_else(|_| {
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
            if let Some(name) = entity.as_str() {
                let _ = memory
                    .relate(name, session_id, "appears_in")
                    .await;
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
pub fn extraction_prompt() -> String {
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
