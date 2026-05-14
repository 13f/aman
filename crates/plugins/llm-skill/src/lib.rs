#![forbid(unsafe_code)]
#![doc = "LLM Skill — processes MESSAGE_RECEIVED events with per-session serial queue."]

use async_trait::async_trait;
use event_bus::EventBus;
use kernel::context::SkillContext;
use kernel::event::{Event, EventType};
use kernel::skill::{Skill, TriggerCondition};
use kernel::validator::{OutputValidator, ValidationOutcome};
use kernel::{AmanResult, Error};
use semver::Version;
use serde_json::json;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};

const DEFAULT_MAX_QUEUE_PER_SESSION: usize = 10;
const SIMULATED_LLM_DELAY_MS: u64 = 100;

/// LLM Skill with per-session serial event processing.
///
/// Each unique `session_id` extracted from `MESSAGE_RECEIVED` event payload
/// gets its own `mpsc::channel` and a background task that processes events
/// one at a time. If a session's queue reaches capacity, new messages are
/// dropped and a `message_dropped` event is published.
pub struct LlmSkill {
    name: String,
    version: Version,
    description: String,
    triggers: Vec<TriggerCondition>,
    bus: Arc<dyn EventBus>,
    sessions: Arc<Mutex<HashMap<String, mpsc::Sender<Event>>>>,
    max_queue: usize,
    /// Set of event UUIDs already processed (for WAL replay dedup, §9.2).
    processed_events: Arc<Mutex<HashSet<String>>>,
    /// OutputValidator for LLM reply security checks (§8.2).
    validator: OutputValidator,
}

impl LlmSkill {
    /// Create a new LLM Skill with the default max queue depth (10 per session).
    #[must_use]
    pub fn new(bus: Arc<dyn EventBus>) -> Self {
        Self::with_config(bus, DEFAULT_MAX_QUEUE_PER_SESSION)
    }

    /// Create a new LLM Skill with a custom max queue depth per session.
    #[must_use]
    pub fn with_config(bus: Arc<dyn EventBus>, max_queue_per_session: usize) -> Self {
        Self {
            name: "llm-skill".to_owned(),
            version: Version::new(0, 1, 0),
            description: "Processes messages via LLM with per-session serial queue".to_owned(),
            triggers: vec![TriggerCondition {
                event_types: vec![EventType::MessageReceived],
                ..Default::default()
            }],
            bus,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            max_queue: max_queue_per_session.max(1),
            processed_events: Arc::new(Mutex::new(HashSet::new())),
            validator: OutputValidator::new(),
        }
    }

    /// Maximum number of queued messages per session.
    #[must_use]
    pub fn max_queue_per_session(&self) -> usize {
        self.max_queue
    }

    /// Drain all active sessions: close every per-session channel.
    /// Returns the number of sessions that were active.
    /// Background `process_session` tasks exit naturally when all senders
    /// are dropped and their `rx.recv()` returns `None`.
    pub fn drain_sessions(&self) -> usize {
        let mut sessions = self.sessions.lock().expect("sessions lock");
        let count = sessions.len();
        sessions.clear(); // drop all senders → receivers get None → tasks exit
        count
    }
}

#[async_trait]
impl Skill for LlmSkill {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &Version {
        &self.version
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn triggers(&self) -> &[TriggerCondition] {
        &self.triggers
    }

    fn drain(&self) -> usize {
        self.drain_sessions()
    }

    async fn execute(&self, mut event: Event, ctx: SkillContext) -> AmanResult<()> {
        // Snapshot SOUL at interaction boundary (§5.2 of architect doc):
        // capture the current SOUL name and system prompt so the entire
        // interaction unit (tool calls → final reply) uses a consistent snapshot.
        if let Some(soul_name) = &ctx.soul_name {
            event.payload["soul_name"] = json!(soul_name);
        }
        if let Some(soul_prompt) = ctx.base.extensions.get("soul.system_prompt") {
            event.payload["soul_system_prompt"] = soul_prompt.clone();
        }

        let session_id = event
            .payload
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::ConfigInvalid {
                message: "MESSAGE_RECEIVED event missing `session_id`".to_owned(),
            })?
            .to_owned();

        let event_id = event.id;

        // UUIDv7 dedup check (§9.2): skip events already processed (WAL replay).
        {
            let mut processed = self.processed_events.lock().expect("processed_events lock");
            if !processed.insert(event_id.to_string()) {
                // Already processed this event — skip (idempotent replay).
                return Ok(());
            }
        }

        // Get or create session sender under lock.
        let sender = {
            let mut sessions = self.sessions.lock().expect("sessions lock");

            match sessions.entry(session_id.clone()) {
                Entry::Occupied(mut entry) => {
                    if entry.get().is_closed() {
                        // Session processor ended; replace with new channel.
                        let (tx, rx) = mpsc::channel(self.max_queue);
                        spawn_session_processor(rx, self.bus.clone(), session_id.clone(), self.validator.clone());
                        entry.insert(tx.clone());
                        tx
                    } else {
                        entry.get().clone()
                    }
                }
                Entry::Vacant(entry) => {
                    let (tx, rx) = mpsc::channel(self.max_queue);
                    spawn_session_processor(rx, self.bus.clone(), session_id.clone(), self.validator.clone());
                    entry.insert(tx.clone());
                    tx
                }
            }
        };

        // Try to enqueue (lock-free).
        match sender.try_send(event) {
            Ok(()) => {
                let _ = self
                    .bus
                    .publish(Event::new(
                        "skill:llm",
                        EventType::Custom("message_queued".to_owned()),
                        json!({
                            "session_id": session_id,
                            "message_id": event_id.to_string(),
                        }),
                    ))
                    .await;
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(event)) => {
                let _ = self
                    .bus
                    .publish(Event::new(
                        "skill:llm",
                        EventType::Custom("message_dropped".to_owned()),
                        json!({
                            "session_id": session_id,
                            "dropped_message_id": event.id.to_string(),
                            "reason": "queue_full",
                        }),
                    ))
                    .await;
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_event)) => {
                // Channel was just replaced — practically impossible but handle gracefully.
                tracing::warn!(%session_id, "session channel closed immediately after creation");
                Ok(())
            }
        }
    }
}

/// Background task that processes messages for a single session sequentially.
async fn process_session(
    mut rx: mpsc::Receiver<Event>,
    bus: Arc<dyn EventBus>,
    session_id: String,
    validator: OutputValidator,
) {
    while let Some(msg) = rx.recv().await {
        // Simulate LLM processing delay.
        sleep(Duration::from_millis(SIMULATED_LLM_DELAY_MS)).await;

        let text = msg
            .payload
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or(""); // TODO: Replace with real LLM call (T4.3).

        // Extract SOUL snapshot captured at interaction boundary.
        let soul_name = msg
            .payload
            .get("soul_name")
            .and_then(|v| v.as_str())
            .unwrap_or("assistant");

        let reply_text = format!("Echo: {text}");

        // OutputValidator check (§8.2): validate complete reply before publishing.
        let outcome = validator.validate(&reply_text);
        match outcome {
            ValidationOutcome::Pass => {
                let reply = Event::new(
                    "skill:llm",
                    EventType::Custom("llm_reply_ready".to_owned()),
                    json!({
                        "session_id": session_id,
                        "original_message_id": msg.id.to_string(),
                        "reply": reply_text,
                        "soul_name": soul_name,
                    }),
                );
                if let Err(e) = bus.publish(reply).await {
                    tracing::warn!(%session_id, error = %e, "llm-skill: failed to publish reply");
                }
            }
            ValidationOutcome::Fail { matched_rules, reason } => {
                tracing::warn!(%session_id, matched = ?matched_rules, "llm-skill: output validation failed");
                let blocked = Event::new(
                    "skill:llm",
                    EventType::Custom("output_blocked".to_owned()),
                    json!({
                        "session_id": session_id,
                        "original_message_id": msg.id.to_string(),
                        "reason": reason,
                        "matched_rules": matched_rules,
                        "soul_name": soul_name,
                    }),
                );
                let _ = bus.publish(blocked).await;
            }
            ValidationOutcome::Error { message } => {
                tracing::warn!(%session_id, error = %message, "llm-skill: validator error (fail_closed)");
                let blocked = Event::new(
                    "skill:llm",
                    EventType::Custom("output_blocked".to_owned()),
                    json!({
                        "session_id": session_id,
                        "original_message_id": msg.id.to_string(),
                        "reason": format!("validator_error: {message}"),
                        "fail_closed": true,
                        "soul_name": soul_name,
                    }),
                );
                let _ = bus.publish(blocked).await;
            }
        }
    }
}

fn spawn_session_processor(
    rx: mpsc::Receiver<Event>,
    bus: Arc<dyn EventBus>,
    session_id: String,
    validator: OutputValidator,
) {
    tokio::spawn(async move {
        process_session(rx, bus, session_id, validator).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::context::BaseContext;
    use kernel::types::TraceId;
    use test_utils::fake_event_bus::{FakeBusConfig, FakeEventBus};

    fn make_message_event(session_id: &str, text: &str) -> Event {
        Event::new(
            "chat:platform",
            EventType::MessageReceived,
            json!({
                "session_id": session_id,
                "text": text,
            }),
        )
    }

    fn skill_context() -> SkillContext {
        SkillContext {
            base: BaseContext::new(TraceId::new()),
            skill_name: Some("llm-skill".to_owned()),
            soul_name: None,
        }
    }

    #[tokio::test]
    async fn accepts_message_received_event() {
        let bus = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
        let skill = LlmSkill::new(bus.clone());
        let event = make_message_event("session-1", "hello");
        let ctx = skill_context();

        skill
            .execute(event, ctx)
            .await
            .expect("execute should succeed");

        // Should have published message_queued
        let queued = bus.events_matching(|e| {
            e.event_type == EventType::Custom("message_queued".to_owned())
        });
        assert_eq!(queued.len(), 1, "should publish message_queued");
        assert_eq!(
            queued[0].payload["session_id"].as_str(),
            Some("session-1")
        );

        // Should eventually produce an LLM reply
        sleep(Duration::from_millis(200)).await;
        let replies = bus.events_matching(|e| {
            e.event_type == EventType::Custom("llm_reply_ready".to_owned())
        });
        assert_eq!(replies.len(), 1, "should produce one LLM reply");
        assert_eq!(
            replies[0].payload["reply"].as_str(),
            Some("Echo: hello")
        );
    }

    #[tokio::test]
    async fn rejects_event_without_session_id() {
        let bus = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
        let skill = LlmSkill::new(bus.clone());
        let event = Event::new("chat:platform", EventType::MessageReceived, json!({"text": "hi"}));
        let ctx = skill_context();

        let err = skill.execute(event, ctx).await.expect_err("missing session_id");
        assert!(matches!(err, Error::ConfigInvalid { .. }));
        assert!(err.to_string().contains("session_id"));
    }

    #[tokio::test]
    async fn drops_message_when_session_queue_is_full() {
        let bus = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
        // max_queue = 1 so the second message is dropped (first is being processed by the
        // background task, occupying the channel capacity).
        let skill = LlmSkill::with_config(bus.clone(), 1);
        let ctx = skill_context();

        // First message occupies the channel.
        skill
            .execute(make_message_event("busy-session", "first"), ctx.clone())
            .await
            .expect("first message accepted");

        // Wait a tiny bit so the background task picks up the first message,
        // emptying the channel slot.
        sleep(Duration::from_millis(20)).await;

        // Second message fills the one remaining slot.
        skill
            .execute(make_message_event("busy-session", "second"), ctx.clone())
            .await
            .expect("second message accepted");

        // Third message should be dropped (queue full).
        skill
            .execute(make_message_event("busy-session", "third"), ctx.clone())
            .await
            .expect("third message handled (dropped)");

        let dropped = bus.events_matching(|e| {
            e.event_type == EventType::Custom("message_dropped".to_owned())
        });
        assert_eq!(dropped.len(), 1, "one message should be dropped");
        assert_eq!(
            dropped[0].payload["reason"].as_str(),
            Some("queue_full")
        );
    }

    #[tokio::test]
    async fn processes_messages_sequentially_per_session() {
        let bus = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
        let skill = LlmSkill::new(bus.clone());
        let ctx = skill_context();

        // Send two messages for the same session.
        skill
            .execute(make_message_event("seq-session", "first"), ctx.clone())
            .await
            .expect("first");
        skill
            .execute(make_message_event("seq-session", "second"), ctx.clone())
            .await
            .expect("second");

        // Wait for both to be processed (2 * 100ms + margin).
        sleep(Duration::from_millis(300)).await;

        let replies = bus.events_matching(|e| {
            e.event_type == EventType::Custom("llm_reply_ready".to_owned())
        });
        assert_eq!(replies.len(), 2, "both messages should produce replies");
        assert_eq!(replies[0].payload["reply"].as_str(), Some("Echo: first"));
        assert_eq!(replies[1].payload["reply"].as_str(), Some("Echo: second"));
    }

    #[tokio::test]
    async fn different_sessions_processed_independently() {
        let bus = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
        let skill = LlmSkill::new(bus.clone());
        let ctx = skill_context();

        skill
            .execute(make_message_event("session-a", "from a"), ctx.clone())
            .await
            .expect("session a");
        skill
            .execute(make_message_event("session-b", "from b"), ctx.clone())
            .await
            .expect("session b");

        sleep(Duration::from_millis(200)).await;

        let replies = bus.events_matching(|e| {
            e.event_type == EventType::Custom("llm_reply_ready".to_owned())
        });
        assert_eq!(replies.len(), 2);
        assert!(replies.iter().any(|e| e.payload["reply"] == json!("Echo: from a")));
        assert!(replies.iter().any(|e| e.payload["reply"] == json!("Echo: from b")));
    }

    #[tokio::test]
    async fn metadata_defaults() {
        let skill = LlmSkill::new(Arc::new(FakeEventBus::new(FakeBusConfig::default())));
        assert_eq!(skill.name(), "llm-skill");
        assert_eq!(skill.version(), &Version::new(0, 1, 0));
        assert!(!skill.description().is_empty());
        assert_eq!(skill.triggers().len(), 1);
        assert_eq!(
            skill.triggers()[0].event_types,
            vec![EventType::MessageReceived]
        );
        assert_eq!(skill.max_queue_per_session(), 10);
    }

    #[tokio::test]
    async fn custom_max_queue_config() {
        let bus = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
        let skill = LlmSkill::with_config(bus, 5);
        assert_eq!(skill.max_queue_per_session(), 5);
    }
}
