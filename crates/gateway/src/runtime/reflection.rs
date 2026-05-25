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
use kernel::AmanResult;
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

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
            "Reflection: QueueDrained received, starting session_extract"
        );

        // Extract one unreflected session per cycle
        self.session_extract(agent_id).await;

        Ok(())
    }
}
