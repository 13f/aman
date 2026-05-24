// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Reflection runner — triggered by Idle(sleep) events during cognitive
//! housekeeping. Extracts structured summaries from one unreflected session
//! per sleep cycle via LLM and stores them in the memory provider for
//! long-term retention.

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

use super::session_store::SessionStore;

/// Handles QueueDrained → reflection logic for all agents.
///
/// Subscribes to the global event bus and processes QueueDrained events from
/// all agents. Dependencies are injected via the `OnceLock` pattern (same as
/// `ReadSkillTool`).
pub struct ReflectionRunner {
    session_store: OnceLock<Arc<SessionStore>>,
    memory_provider: OnceLock<Arc<dyn MemoryProvider>>,
    llm_provider: OnceLock<Arc<dyn LlmProvider>>,
    memory_llm: OnceLock<MemoryLlmConfig>,
}

impl ReflectionRunner {
    pub fn new() -> Self {
        Self {
            session_store: OnceLock::new(),
            memory_provider: OnceLock::new(),
            llm_provider: OnceLock::new(),
            memory_llm: OnceLock::new(),
        }
    }

    pub fn set_session_store(&self, store: Arc<SessionStore>) {
        let _ = self.session_store.set(store);
    }

    pub fn set_memory_provider(&self, provider: Arc<dyn MemoryProvider>) {
        let _ = self.memory_provider.set(provider);
    }

    pub fn set_llm_provider(&self, provider: Arc<dyn LlmProvider>) {
        let _ = self.llm_provider.set(provider);
    }

    pub fn set_memory_llm(&self, config: MemoryLlmConfig) {
        let _ = self.memory_llm.set(config);
    }

    // -- session_extract ------------------------------------------------------

    /// Query one unreflected session, extract structured summary via LLM, and
    /// store in the memory provider. Mark the session as reflected on success.
    /// Processes at most one session per invocation.
    async fn session_extract(&self, agent_id: &str) {
        let Some(store) = self.session_store.get() else {
            debug!("Reflection: no SessionStore, skipping session_extract");
            return;
        };
        let Some(memory) = self.memory_provider.get() else {
            debug!("Reflection: no MemoryProvider, skipping session_extract");
            return;
        };
        let Some(llm) = self.llm_provider.get() else {
            debug!("Reflection: no LlmProvider, skipping session_extract");
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

        // Load persisted events from JSONL
        let events = store.load_session_events(&session.id);
        if events.len() < 2 {
            // Mark as reflected even if not enough content — don't retry forever
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64;
            let _ = store.mark_reflected(&session.id, now);
            return;
        }

        debug!(
            agent_id,
            session_id = %session.id,
            event_count = events.len(),
            "Reflection: extracting session",
        );

        match self.extract_and_store(llm, memory, agent_id, &session.id, &events).await {
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
    ) -> AmanResult<()> {
        // Build a compact representation of the conversation
        let conversation = Self::format_conversation(events);
        let system_prompt = Self::extraction_prompt();

        let llm_config = self.memory_llm.get();
        let model = llm_config
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
    fn format_conversation(events: &[serde_json::Value]) -> String {
        let mut out = String::new();
        for event in events {
            let event_type = event
                .get("event_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let payload = event.get("payload").map(|p| p.to_string()).unwrap_or_default();
            // Truncate large payloads
            let payload = if payload.len() > 2000 {
                format!("{}…(truncated)", &payload[..2000])
            } else {
                payload
            };
            out.push_str(&format!("[{event_type}] {payload}\n"));
        }
        out
    }

    /// System prompt for session extraction.
    fn extraction_prompt() -> String {
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
}

#[async_trait]
impl EventHandler for ReflectionRunner {
    async fn handle(&self, event: Event) -> AmanResult<()> {
        // Only process Idle events with kind=sleep
        if event.event_type != EventType::Idle {
            return Ok(());
        }

        let kind = event
            .payload
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if kind != "sleep" {
            return Ok(());
        }

        let agent_id = event
            .payload
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        debug!(
            agent_id,
            "Reflection: Idle(sleep) received"
        );

        // Extract one unreflected session per sleep cycle
        self.session_extract(agent_id).await;

        Ok(())
    }
}
