// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Session manager — orchestrates session lifecycle, persistence, and message routing.
//!
//! Extracted from HTTP handlers and `AgentRuntime` so that session state machine,
//! OCC, persistence, and system prompt caching are independent of the chat transport.

pub mod prompt_cache;
pub mod work_session;

use std::sync::Arc;

use kernel::event::{Event, EventType};
use kernel::AmanResult;
use serde_json::json;
use workflow::{StateDef, StateTimeout, Transition, TransitionFrom, TransitionTo, WorkflowDef};

use super::agent_harness::AgentHarness;
use super::agent_registry::AgentRegistry;
use super::audit::AuditLogger;
use super::session_store;
use crate::runtime::session::prompt_cache::SystemPromptCache;

// ── SessionManager ───────────────────────────────────────────────────────────

/// Coordinates session lifecycle across the workflow engine, agent harness,
/// event bus, session stores, and system prompt cache.
pub struct SessionManager {
    workflow_engine: Arc<workflow::WorkflowEngine>,
    agent_registry: Arc<AgentRegistry>,
    agent_harness: Arc<AgentHarness>,
    bus: Arc<dyn event_bus::EventBus>,
    audit: Arc<AuditLogger>,
    prompt_cache: SystemPromptCache,
}

impl SessionManager {
    pub fn new(
        workflow_engine: Arc<workflow::WorkflowEngine>,
        agent_registry: Arc<AgentRegistry>,
        agent_harness: Arc<AgentHarness>,
        bus: Arc<dyn event_bus::EventBus>,
        audit: Arc<AuditLogger>,
    ) -> Self {
        Self {
            workflow_engine,
            agent_registry,
            agent_harness,
            bus,
            audit,
            prompt_cache: SystemPromptCache::new(),
        }
    }
}

// ── Workflow registration ────────────────────────────────────────────────────

impl SessionManager {
    /// Register the session state machine on the given workflow engine.
    ///
    /// Called once during gateway startup.
    pub fn register_workflow(engine: &workflow::WorkflowEngine) {
        if let Err(e) = engine.register_workflow(WorkflowDef {
            name: "message-session".to_owned(),
            states: vec![
                StateDef { name: "ACTIVE".to_owned() },
                StateDef { name: "PROCESSING".to_owned() },
                StateDef { name: "IDLE".to_owned() },
                StateDef { name: "ERROR".to_owned() },
                StateDef { name: "RETRYING".to_owned() },
                StateDef { name: "TIMEOUT".to_owned() },
                StateDef { name: "CLOSED".to_owned() },
            ],
            initial_state: "ACTIVE".to_owned(),
            final_states: vec!["CLOSED".to_owned()],
            error_state: "ERROR".to_owned(),
            transitions: vec![
                Transition {
                    from: TransitionFrom::Specific("ACTIVE".to_owned()),
                    event: "MESSAGE_RECEIVED".to_owned(),
                    to: TransitionTo::Specific("PROCESSING".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("ACTIVE".to_owned()),
                    event: "SESSION_TIMEOUT".to_owned(),
                    to: TransitionTo::Specific("TIMEOUT".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("PROCESSING".to_owned()),
                    event: "LLM_REPLY_READY".to_owned(),
                    to: TransitionTo::Specific("IDLE".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("PROCESSING".to_owned()),
                    event: "LLM_STREAM_DONE".to_owned(),
                    to: TransitionTo::Specific("IDLE".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("PROCESSING".to_owned()),
                    event: "LLM_ERROR".to_owned(),
                    to: TransitionTo::Specific("ERROR".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("PROCESSING".to_owned()),
                    event: "STREAM_TIMEOUT".to_owned(),
                    to: TransitionTo::Specific("TIMEOUT".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("PROCESSING".to_owned()),
                    event: "SESSION_CLOSE_CMD".to_owned(),
                    to: TransitionTo::Specific("CLOSED".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("IDLE".to_owned()),
                    event: "MESSAGE_RECEIVED".to_owned(),
                    to: TransitionTo::Specific("PROCESSING".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("IDLE".to_owned()),
                    event: "SESSION_TIMEOUT".to_owned(),
                    to: TransitionTo::Specific("TIMEOUT".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("IDLE".to_owned()),
                    event: "SESSION_END".to_owned(),
                    to: TransitionTo::Specific("CLOSED".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("ERROR".to_owned()),
                    event: "RETRY_CMD".to_owned(),
                    to: TransitionTo::Specific("RETRYING".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("ERROR".to_owned()),
                    event: "SESSION_END".to_owned(),
                    to: TransitionTo::Specific("IDLE".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("ERROR".to_owned()),
                    event: "ABANDON_TIMEOUT".to_owned(),
                    to: TransitionTo::Specific("CLOSED".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("RETRYING".to_owned()),
                    event: "RETRY_STARTED".to_owned(),
                    to: TransitionTo::Specific("PROCESSING".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("RETRYING".to_owned()),
                    event: "RETRY_FAILED".to_owned(),
                    to: TransitionTo::Specific("ERROR".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("TIMEOUT".to_owned()),
                    event: "SESSION_END".to_owned(),
                    to: TransitionTo::Specific("CLOSED".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
                Transition {
                    from: TransitionFrom::Specific("TIMEOUT".to_owned()),
                    event: "MESSAGE_RECEIVED".to_owned(),
                    to: TransitionTo::Specific("IDLE".to_owned()),
                    guard: None, on_fail: None, action: None, on_action_failure: None,
                },
            ],
            state_timeouts: vec![
                StateTimeout {
                    state: "ACTIVE".to_owned(),
                    timeout_ms: 300_000,
                    on_timeout: TransitionTo::Specific("TIMEOUT".to_owned()),
                    on_timeout_alert: None,
                },
                StateTimeout {
                    state: "PROCESSING".to_owned(),
                    timeout_ms: 120_000,
                    on_timeout: TransitionTo::Specific("TIMEOUT".to_owned()),
                    on_timeout_alert: None,
                },
                StateTimeout {
                    state: "IDLE".to_owned(),
                    timeout_ms: 600_000,
                    on_timeout: TransitionTo::Specific("TIMEOUT".to_owned()),
                    on_timeout_alert: None,
                },
                StateTimeout {
                    state: "ERROR".to_owned(),
                    timeout_ms: 120_000,
                    on_timeout: TransitionTo::Specific("CLOSED".to_owned()),
                    on_timeout_alert: None,
                },
                StateTimeout {
                    state: "TIMEOUT".to_owned(),
                    timeout_ms: 120_000,
                    on_timeout: TransitionTo::Specific("CLOSED".to_owned()),
                    on_timeout_alert: None,
                },
            ],
            error_recovery: workflow::ErrorRecovery {
                auto_retry_count: 3,
                max_retry_count: 5,
                on_retry_failure: workflow::RetryFailurePolicy::ManualOnly,
                retry_backoff: kernel::retry::RetryBackoff::Fixed(1000),
            },
        }) {
            tracing::warn!(error = %e, "failed to register message-session workflow; session state machine unavailable");
        }
    }
}

// ── Session restoration ──────────────────────────────────────────────────────

impl SessionManager {
    /// Restore a persisted session from JSONL events (e.g. after gateway restart).
    ///
    /// Searches all per-agent session stores for `session_id`, loads the event log,
    /// rebuilds conversation history in the agent harness, and re-registers the
    /// workflow instance.
    pub async fn restore_session(&self, session_id: &str) -> Option<()> {
        // Collect every store that knows about this session, along with its
        // events and indexed agent_id.  After a gateway restart the session
        // may exist in more than one agent's DB and JSONL directory — we
        // need to pick the *correct* owner.
        struct Candidate {
            events: Vec<serde_json::Value>,
            db_agent_id: String,
        }

        let stores = self.agent_registry.all_session_stores().await;
        let mut candidates: Vec<Candidate> = Vec::new();
        for s in &stores {
            if !s.has_session(session_id) {
                continue;
            }
            let events = s.load_session_events(session_id);
            let db_agent_id = s
                .get(session_id)
                .ok()
                .flatten()
                .map(|rec| rec.agent_id)
                .unwrap_or_default();
            candidates.push(Candidate {
                events,
                db_agent_id,
            });
        }

        // Best candidate: the one whose JSONL actually has events;
        // among those, prefer the one whose DB agent_id agrees with the
        // agent_id carried in the event payloads (the ground truth).
        // Break ties with event count — the store that has more persisted
        // events is more likely to be the correct owner.
        let best = candidates
            .into_iter()
            .filter(|c| !c.events.is_empty())
            .max_by(|a, b| {
                // Find agent_id from events — prefer MessageReceived events
                // (which carry the original creation agent_id from instance data)
                // over reply events (which carry the processing agent's id).
                let event_aid = |c: &Candidate| -> Option<String> {
                    // First pass: look for agent_id in MessageReceived events
                    // (these carry the original creation agent_id).
                    for e in &c.events {
                        let et = e["event_type"].as_str().unwrap_or("");
                        if et.contains("MessageReceived")
                            && let Some(aid) = e["payload"]["agent_id"].as_str().filter(|id| !id.is_empty()) {
                                return Some(aid.to_owned());
                            }
                    }
                    // Second pass: any event with an agent_id.
                    for e in &c.events {
                        if let Some(aid) = e["payload"]["agent_id"].as_str().filter(|id| !id.is_empty()) {
                            return Some(aid.to_owned());
                        }
                    }
                    None
                };
                let has_msg_recv = |c: &Candidate| {
                    c.events.iter().any(|e| {
                        e["event_type"].as_str().is_some_and(|et| et.contains("MessageReceived"))
                    })
                };
                let score = |c: &Candidate| -> (u32, u32, usize) {
                    let ea = event_aid(c);
                    let s: u32 = match ea {
                        Some(aid) if aid == c.db_agent_id => 2,
                        None if !c.db_agent_id.is_empty() => 1,
                        Some(_) => 1,
                        _ => 0,
                    };
                    // Prefer the candidate that has MessageReceived events
                    // — they carry the original creation agent_id.
                    let has_msg: u32 = if has_msg_recv(c) { 1 } else { 0 };
                    (s, has_msg, c.events.len())
                };
                let (a_s, a_m, a_n) = score(a);
                let (b_s, b_m, b_n) = score(b);
                a_s.cmp(&b_s).then_with(|| a_m.cmp(&b_m)).then_with(|| a_n.cmp(&b_n))
            })?;

        let events = best.events;

        // Resolve agent_id: MessageReceived events carry the original
        // creation agent_id from instance data — that's the ground truth.
        let agent_id = events
            .iter()
            .filter(|e| {
                e["event_type"].as_str().is_some_and(|et| et.contains("MessageReceived"))
            })
            .find_map(|e| {
                e["payload"]["agent_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .map(String::from)
            })
            .or_else(|| {
                events.iter().find_map(|e| {
                    e["payload"]["agent_id"]
                        .as_str()
                        .filter(|id| !id.is_empty())
                        .map(String::from)
                })
            })
            .or({
                if best.db_agent_id.is_empty() {
                    None
                } else {
                    Some(best.db_agent_id)
                }
            })
            .unwrap_or_else(|| "aman".to_owned());

        self.agent_harness.restore_session_history(session_id, &events);

        let first_ts = events.first().and_then(|e| e["timestamp_ms"].as_i64()).unwrap_or(0);
        let last_ts = events.last().and_then(|e| e["timestamp_ms"].as_i64()).unwrap_or(0);
        let msg_count = events.iter().filter(|e| {
            e["event_type"].as_str().is_some_and(|et| {
                et == "MessageReceived" || et.contains("reply_ready") || et == "llm_reply_ready"
            })
        }).count() as u64;

        let data = serde_json::json!({
            "session_type": "persistent",
            "agent_id": agent_id,
            "version": events.len() as u64,
            "message_count": msg_count,
            "created_at": first_ts,
            "last_active_at": last_ts,
        });

        self.workflow_engine
            .restore_instance(session_id, "message-session", data)
            .ok()?;

        Some(())
    }
}


// ── Session creation ─────────────────────────────────────────────────────────

impl SessionManager {
    /// Create a new session workflow instance, publish `session:started`, and
    /// persist to the session store. Returns the new session ID.
    pub async fn create_session(
        &self,
        operator: &str,
        agent_id: &str,
        session_type: &str,
    ) -> AmanResult<String> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let data = json!({
            "session_type": session_type,
            "agent_id": agent_id,
            "version": 0,
            "created_at": now_ms,
            "last_active_at": now_ms,
        });

        let instance = self.workflow_engine.create_instance("message-session", data)?;
        let id = instance.id.clone();

        self.audit.record(operator, "chat.session.create", format!("session:{id}"), "ok", "");

        let session_started_event = Event::new(
            "session:control",
            EventType::Custom("session:started".to_owned()),
            json!({
                "session_id": id,
                "session_type": session_type,
                "operator": operator,
            }),
        );

        // Publish to global bus.
        if let Err(e) = self.bus.publish(session_started_event.clone()).await {
            tracing::warn!(session_id = %id, agent_id = %agent_id, error = %e, "failed to publish session:started event");
        }

        // Persist to the agent's session store.
        let created_at = instance.data.get("created_at")
            .and_then(|v| v.as_i64()).unwrap_or(0);
        let last_active_at = instance.data.get("last_active_at")
            .and_then(|v| v.as_i64()).unwrap_or(0);
        if let Some(store) = self.agent_registry.get_session_store(agent_id).await {
            if let Err(e) = store.upsert(&session_store::SessionRecord {
                id: id.clone(),
                agent_id: agent_id.to_owned(),
                state: instance.current_state,
                message_count: 0,
                created_at,
                last_active_at,
                session_type: session_type.to_owned(),
                reflected_at: None,
                title: None,
            }) {
                tracing::warn!(session_id = %id, agent_id = %agent_id, error = %e, "failed to persist new session record");
            }
        }

        Ok(id)
    }

    /// Ensure a session workflow instance and record exist for a given session_id.
    ///
    /// If the session already exists in any agent's store, this is a no-op.
    /// Otherwise, creates the workflow instance (via `restore_instance` so the
    /// caller controls the session_id) and persists a `SessionRecord`.
    ///
    /// This is idempotent — safe to call before every message for sessions that
    /// may not have been created through the normal `create_session` path (e.g.
    /// boredom-triggered background sessions, work-item sessions).
    pub async fn ensure_session(
        &self,
        session_id: &str,
        agent_id: &str,
        session_type: &str,
    ) -> AmanResult<()> {
        // Check all stores — session may belong to a different agent
        let stores = self.agent_registry.all_session_stores().await;
        for s in &stores {
            if s.has_session(session_id) {
                return Ok(());
            }
        }

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let data = json!({
            "session_type": session_type,
            "agent_id": agent_id,
            "version": 0,
            "created_at": now_ms,
            "last_active_at": now_ms,
        });

        // Use restore_instance so we control the session_id (needed for
        // deterministic work-item session IDs and boredom run IDs).
        let instance = self
            .workflow_engine
            .restore_instance(session_id, "message-session", data)?;

        self.audit.record(
            &format!("system:session_{session_type}"),
            "chat.session.create",
            format!("session:{session_id}"),
            "ok",
            "",
        );

        // Publish session:started event
        if let Err(e) = self
            .bus
            .publish(Event::new(
                "session:control",
                EventType::Custom("session:started".to_owned()),
                json!({
                    "session_id": session_id,
                    "session_type": session_type,
                    "operator": "system",
                }),
            ))
            .await
        {
            tracing::warn!(session_id = %session_id, agent_id = %agent_id, error = %e, "failed to publish session:started event");
        }

        // Persist to the agent's session store
        if let Some(store) = self.agent_registry.get_session_store(agent_id).await {
            if let Err(e) = store.upsert(&session_store::SessionRecord {
                id: session_id.to_owned(),
                agent_id: agent_id.to_owned(),
                state: instance.current_state,
                message_count: 0,
                created_at: now_ms as i64,
                last_active_at: now_ms as i64,
                session_type: session_type.to_owned(),
                reflected_at: None,
                title: None,
            }) {
                tracing::warn!(session_id = %session_id, agent_id = %agent_id, error = %e, "failed to persist session record");
            }
        }

        Ok(())
    }

    /// Handle a completed agent reply: transition the workflow engine from
    /// PROCESSING to IDLE and update the SQLite session record.
    pub async fn handle_reply(
        &self,
        session_id: &str,
        agent_id: &str,
        _reply: &str,
    ) {
        // Transition workflow engine: PROCESSING → IDLE
        let transition_event = Event::new(
            "session:control",
            EventType::Custom("LLM_REPLY_READY".to_owned()),
            json!({
                "session_id": session_id,
                "agent_id": agent_id,
            }),
        );
        if let Err(e) = self.workflow_engine.handle_event(session_id, transition_event).await {
            tracing::warn!(session_id = %session_id, error = %e, "failed to transition session workflow state");
        }

        // Update instance metadata
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        if let Err(e) = self.workflow_engine.update_instance_data(session_id, |data| {
            data["last_active_at"] = json!(now_ms);
            let mc = data.get("message_count").and_then(|v| v.as_u64()).unwrap_or(0);
            data["message_count"] = json!(mc + 1);
        }) {
            tracing::warn!(session_id = %session_id, error = %e, "failed to update workflow instance data");
        }

        // Persist updated session record to SQLite
        if let Some(store) = self.agent_registry.get_session_store(agent_id).await
            && let Some(inst) = self.workflow_engine.get_instance(session_id) {
                let session_type = inst.data.get("session_type")
                    .and_then(|v| v.as_str()).unwrap_or("persistent");
                let created_at = inst.data.get("created_at")
                    .and_then(|v| v.as_i64()).unwrap_or(0);
                let message_count = inst.data.get("message_count")
                    .and_then(|v| v.as_i64()).unwrap_or(0);
                if let Err(e) = store.upsert(&session_store::SessionRecord {
                    id: inst.id.clone(),
                    agent_id: agent_id.to_owned(),
                    state: inst.current_state,
                    message_count,
                    created_at,
                    last_active_at: now_ms as i64,
                    session_type: session_type.to_owned(),
                    reflected_at: None,
                title: None,
                }) {
                    tracing::warn!(session_id = %inst.id, agent_id = %agent_id, error = %e, "failed to persist session record after reply");
                }
            }
    }
}

// ── System prompt ────────────────────────────────────────────────────────────

impl SessionManager {
    /// Get or build the cached combined system prompt for `session_id`.
    ///
    /// `build_fn` is only called on cache miss (first turn of the session).
    pub fn get_system_prompt(
        &self,
        session_id: &str,
        build_fn: impl FnOnce() -> String,
    ) -> String {
        self.prompt_cache.get_or_build(session_id, build_fn)
    }

    /// Invalidate the cached system prompt for a session.
    pub fn invalidate_prompt(&self, session_id: &str) {
        self.prompt_cache.invalidate(session_id);
    }
}

// ── Accessors ────────────────────────────────────────────────────────────────

impl SessionManager {
    pub fn workflow_engine(&self) -> &workflow::WorkflowEngine {
        &self.workflow_engine
    }

    pub fn agent_registry(&self) -> &AgentRegistry {
        &self.agent_registry
    }

    pub fn bus(&self) -> &dyn event_bus::EventBus {
        self.bus.as_ref()
    }

    pub fn audit(&self) -> &AuditLogger {
        &self.audit
    }
}
