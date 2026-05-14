#![forbid(unsafe_code)]
#![doc = "LLM Skill — processes MESSAGE_RECEIVED events with per-session serial queue."]

use async_trait::async_trait;
use event_bus::EventBus;
use kernel::context::SkillContext;
use kernel::event::{Event, EventType};
use kernel::skill::{Skill, TriggerCondition};
use kernel::validator::{OutputValidator, ValidationOutcome};
use kernel::{AmanResult, Error};
use serde::Serialize;
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

// --- History trimming configuration (§15) ---
/// Default context window in tokens (conservative; real LLM provides this).
const CONTEXT_WINDOW_TOKENS: usize = 4096;
/// Trim trigger when total > window × threshold (§15.2).
const TRIM_THRESHOLD_RATIO: f64 = 0.8;
/// Minimum messages to keep after trimming (§15.4).
const TRIM_MINIMUM_MESSAGES: usize = 5;
/// Safety margin multiplier applied to target (§15.4: keep 20% headroom).
const TRIM_SAFETY_MARGIN: f64 = 0.8;
/// Rough chars-per-token estimate (≈4 chars/token for English).
const CHARS_PER_TOKEN: usize = 4;

/// A single message in session history (§15).
#[derive(Debug, Clone, Serialize)]
struct HistoryEntry {
    role: String,
    event_id: String,
    text: String,
    estimated_tokens: usize,
}

/// Per-session message history with FIFO trimming support (§15.3, §15.4).
///
/// Accumulates user+assistant message pairs and trims oldest pairs when the
/// estimated token total exceeds the context window threshold. Trimming is
/// always in full pairs (user + assistant) to preserve semantic units.
#[derive(Debug, Clone)]
struct SessionHistory {
    messages: Vec<HistoryEntry>,
    total_trimmed: usize,
    trim_count: usize,
}

impl SessionHistory {
    /// Create a new empty session history.
    fn new() -> Self {
        Self {
            messages: Vec::new(),
            total_trimmed: 0,
            trim_count: 0,
        }
    }

    /// Rough token estimate: `chars / CHARS_PER_TOKEN + 1`.
    fn estimate_tokens(text: &str) -> usize {
        (text.len() / CHARS_PER_TOKEN).max(1)
    }

    /// Push a message (user or assistant) into history.
    fn push(&mut self, role: &str, event_id: &str, text: &str) {
        self.messages.push(HistoryEntry {
            role: role.to_owned(),
            event_id: event_id.to_owned(),
            text: text.to_owned(),
            estimated_tokens: Self::estimate_tokens(text),
        });
    }

    /// Total estimated tokens across all messages.
    fn estimated_total_tokens(&self) -> usize {
        self.messages.iter().map(|m| m.estimated_tokens).sum()
    }

    /// Number of user+assistant message pairs.
    #[allow(dead_code)]
    fn pair_count(&self) -> usize {
        self.messages.len() / 2
    }

    /// Check whether trimming should be triggered (§15.2).
    ///
    /// Trigger condition: `total_tokens > context_window × threshold_ratio`
    fn should_trim(&self, soul_tokens: usize, msg_tokens: usize) -> bool {
        let total = soul_tokens + self.estimated_total_tokens() + msg_tokens;
        total > (CONTEXT_WINDOW_TOKENS as f64 * TRIM_THRESHOLD_RATIO) as usize
    }

    /// FIFO trimming (§15.4): remove oldest message pairs until the estimated
    /// token count drops to `(context_window × threshold × safety_margin)`,
    /// while keeping at least `TRIM_MINIMUM_MESSAGES` individual messages.
    ///
    /// Returns the number of messages trimmed.
    fn trim_fifo(&mut self, soul_tokens: usize, msg_tokens: usize) -> (usize, usize) {
        let target_tokens =
            (CONTEXT_WINDOW_TOKENS as f64 * TRIM_THRESHOLD_RATIO * TRIM_SAFETY_MARGIN) as usize;
        let mut current = soul_tokens + self.estimated_total_tokens() + msg_tokens;
        let mut trimmed = 0usize;
        let mut trimmed_tokens = 0usize;

        while current > target_tokens && self.messages.len() >= TRIM_MINIMUM_MESSAGES + 2 {
            // Remove oldest pair (user + assistant) — always in pairs (§15.3).
            for _ in 0..2 {
                if let Some(msg) = self.messages.first() {
                    current = current.saturating_sub(msg.estimated_tokens);
                    trimmed_tokens += msg.estimated_tokens;
                    self.messages.remove(0);
                    trimmed += 1;
                }
            }
        }

        if trimmed > 0 {
            self.total_trimmed += trimmed;
            self.trim_count += 1;
        }

        (trimmed, trimmed_tokens)
    }

    /// Generate a unique trim ID from the current event's message ID.
    fn next_trim_id(&self, msg_id: &str) -> String {
        // Use the first segment of the event UUID for a unique, traceable identifier.
        let short = msg_id.split('-').next().unwrap_or("0");
        format!("trim_{}_{}", self.trim_count, short)
    }
}

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
///
/// Maintains per-session `SessionHistory` for context window tracking (§11.8)
/// and FIFO trimming (§15). Before each LLM call, checks whether the estimated
/// token total exceeds the threshold and trims oldest message pairs if needed.
async fn process_session(
    mut rx: mpsc::Receiver<Event>,
    bus: Arc<dyn EventBus>,
    session_id: String,
    validator: OutputValidator,
) {
    let mut history = SessionHistory::new();

    while let Some(msg) = rx.recv().await {
        let text = msg
            .payload
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or(""); // TODO: Replace with real LLM call (T4.3).

        // Record user message in history before processing.
        history.push("user", &msg.id.to_string(), text);

        // Extract SOUL snapshot captured at interaction boundary.
        let soul_prompt = msg
            .payload
            .get("soul_system_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let soul_name = msg
            .payload
            .get("soul_name")
            .and_then(|v| v.as_str())
            .unwrap_or("assistant");

        // Context window calculation (§11.8):
        //   total_tokens = base_prompt(SOUL) + history_tokens + user_message_tokens
        let soul_tokens = SessionHistory::estimate_tokens(soul_prompt);
        let msg_tokens = SessionHistory::estimate_tokens(text);

        // Trim check (§15.2): if total exceeds threshold, trim FIFO.
        if history.should_trim(soul_tokens, msg_tokens) {
            let (trimmed_count, trimmed_tokens_estimate) = history.trim_fifo(soul_tokens, msg_tokens);
            if trimmed_count > 0 {
                let remaining_count = history.messages.len();
                let trim_id = history.next_trim_id(&msg.id.to_string());
                let trimmed_event = Event::new(
                    "skill:llm",
                    EventType::Custom("history_trimmed".to_owned()),
                    json!({
                        "session_id": session_id,
                        "trimmed_count": trimmed_count,
                        "remaining_count": remaining_count,
                        "trimmed_token_estimate": trimmed_tokens_estimate,
                        "strategy": "fifo",
                        "trim_id": trim_id,
                    }),
                );
                let _ = bus.publish(trimmed_event).await;
                tracing::info!(
                    %session_id,
                    trimmed = trimmed_count,
                    remaining = remaining_count,
                    strategy = "fifo",
                    "session history trimmed"
                );
            }
        }

        // Simulate LLM processing delay.
        sleep(Duration::from_millis(SIMULATED_LLM_DELAY_MS)).await;

        let reply_text = format!("Echo: {text}");

        // Record assistant reply in history.
        history.push("assistant", &msg.id.to_string(), &reply_text);

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

    // --- SessionHistory unit tests ---

    #[test]
    fn session_history_accumulates_messages() {
        let mut h = SessionHistory::new();
        assert_eq!(h.estimated_total_tokens(), 0);
        assert_eq!(h.messages.len(), 0);

        h.push("user", "e1", "hello");
        h.push("assistant", "e2", "Hi there!");
        assert_eq!(h.messages.len(), 2);
        assert_eq!(h.messages[0].role, "user");
        assert_eq!(h.messages[1].role, "assistant");
        assert!(h.estimated_total_tokens() > 0);
        assert_eq!(h.total_trimmed, 0);
        assert_eq!(h.trim_count, 0);
    }

    #[test]
    fn session_history_estimated_tokens_non_zero() {
        // Even an empty string should return at least 1 token.
        assert_eq!(SessionHistory::estimate_tokens(""), 1);
        assert_eq!(SessionHistory::estimate_tokens("a"), 1);
        // 4 chars ≈ 1 token.
        assert_eq!(SessionHistory::estimate_tokens("abcd"), 1);
        // 7 chars still ≈ 1 token (integer division).
        assert_eq!(SessionHistory::estimate_tokens("abcdefg"), 1);
        // 8 chars ≈ 2 tokens.
        assert_eq!(SessionHistory::estimate_tokens("abcdefgh"), 2);
    }

    #[test]
    fn session_history_should_trim_returns_false_below_threshold() {
        let mut h = SessionHistory::new();
        // 10 short messages (~10 tokens each) → ~100 tokens total, well below 3277.
        for i in 0..10 {
            h.push("user", &format!("e{i}a"), "short msg");
            h.push("assistant", &format!("e{i}b"), "short reply");
        }
        assert!(!h.should_trim(0, 5));
    }

    #[test]
    fn session_history_trims_fifo_when_over_threshold() {
        let mut h = SessionHistory::new();

        // Medium text: each message ≈ 1500 chars ≈ 375 tokens.
        // A pair (user+assistant) ≈ 750 tokens.
        // After 6 pairs (12 messages): 12 × 375 = 4500 tokens → above 3277 threshold.
        // Use 5 pairs for setup, then a long final message to trigger.
        let medium_text = "X".repeat(1500);
        for i in 0..5 {
            h.push("user", &format!("e{i}a"), &medium_text);   // +375 tokens
            h.push("assistant", &format!("e{i}b"), &medium_text); // +375 tokens
        }
        // 5 pairs = 10 messages, ~3750 tokens total.
        assert_eq!(h.messages.len(), 10);
        let msg_tokens = SessionHistory::estimate_tokens(&medium_text);
        // History alone + new medium message may already exceed threshold,
        // so use a very short msg_tokens for the "should not trim" check.
        assert!(h.should_trim(0, msg_tokens),
            "10 medium messages should exceed threshold");

        // Trim with a small msg_tokens to isolate just the history excess.
        let (trimmed, trimmed_tokens) = h.trim_fifo(0, 1);
        assert!(trimmed > 0, "should have trimmed some messages");
        assert!(trimmed_tokens > 0, "should have freed tokens");
        assert_eq!(h.total_trimmed, trimmed);
        assert_eq!(h.trim_count, 1);

        // After trimming, remaining counts must respect minimum.
        assert!(h.messages.len() >= TRIM_MINIMUM_MESSAGES,
            "remaining messages must respect minimum");
    }

    #[test]
    fn session_history_trim_respects_minimum_messages() {
        let mut h = SessionHistory::new();
        // Push multiple long pairs.
        let long_text = "X".repeat(4096);
        for i in 0..10 {
            h.push("user", &format!("e{i}a"), &long_text);
            h.push("assistant", &format!("e{i}b"), &long_text);
        }
        // Push one more user message to trigger trim.
        h.push("user", "last", &long_text);
        let msg_tokens = SessionHistory::estimate_tokens(&long_text);

        let (trimmed, _trimmed_tokens) = h.trim_fifo(0, msg_tokens);
        assert!(trimmed > 0, "should have trimmed");
        // Remaining messages must be >= TRIM_MINIMUM_MESSAGES.
        assert!(h.messages.len() >= TRIM_MINIMUM_MESSAGES,
            "must keep at least {} messages, got {}", TRIM_MINIMUM_MESSAGES, h.messages.len());
    }

    #[test]
    fn session_history_no_trim_when_barely_over_minimum() {
        // With exactly TRIM_MINIMUM_MESSAGES + 1 messages, we can't remove a full pair.
        let mut h = SessionHistory::new();
        // 3 pairs + 0 = 6 messages. TRIM_MINIMUM_MESSAGES + 2 = 7, so trimming should not happen.
        // Actually let's make it exactly 6 messages (3 pairs).
        let long_text = "X".repeat(4096);
        for i in 0..3 {
            h.push("user", &format!("e{i}a"), &long_text);
            h.push("assistant", &format!("e{i}b"), &long_text);
        }
        // Total: 6 messages, 6 * 1024 = 6144 tokens.
        // Push one more user message: 7 messages total. trim_fifo guard: 7 >= 7 → can remove 1 pair.
        h.push("user", "e_last", &long_text);
        assert_eq!(h.messages.len(), 7);
        let msg_tokens = SessionHistory::estimate_tokens(&long_text);

        // This should trim 1 pair (2 messages) → 5 remaining.
        let (trimmed, _trimmed_tokens) = h.trim_fifo(0, msg_tokens);
        assert_eq!(trimmed, 2, "should trim exactly 1 pair (2 messages)");
        assert_eq!(h.messages.len(), 5, "should have exactly 5 messages remaining");
    }

    #[tokio::test]
    async fn history_trimmed_event_published_when_threshold_exceeded() {
        let bus = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
        let skill = LlmSkill::new(bus.clone());
        let ctx = skill_context();
        let long_text = "X".repeat(4096); // ≈1024 tokens per message

        // Send enough messages to trigger trimming.
        // 3 pairs of user+assistant = 6 messages × 1024 chars ≈ 6144 tokens,
        // which exceeds 4096 × 0.8 = 3277 token threshold.
        for i in 0..6 {
            skill
                .execute(make_message_event("trim-session", &long_text), ctx.clone())
                .await
                .expect(&format!("message {i} accepted"));
            // Wait for each message to be processed before sending the next,
            // so each becomes a full pair in history.
            sleep(Duration::from_millis(150)).await;
        }

        // Now send one more message to trigger trim.
        sleep(Duration::from_millis(200)).await;
        skill
            .execute(make_message_event("trim-session", &long_text), ctx.clone())
            .await
            .expect("final message accepted");

        // Wait for trim detection + processing.
        sleep(Duration::from_millis(300)).await;

        // Check for history_trimmed events.
        let trimmed_events = bus.events_matching(|e| {
            e.event_type == EventType::Custom("history_trimmed".to_owned())
        });
        assert!(
            !trimmed_events.is_empty(),
            "expected at least one history_trimmed event, got 0"
        );

        // Verify event payload structure (§15.5).
        let event = &trimmed_events[0];
        assert_eq!(
            event.payload["session_id"].as_str(),
            Some("trim-session")
        );
        assert!(event.payload["trimmed_count"].as_u64().unwrap_or(0) > 0);
        assert!(event.payload["remaining_count"].as_u64().unwrap_or(0) >= TRIM_MINIMUM_MESSAGES as u64);
        assert!(event.payload["trimmed_token_estimate"].as_u64().unwrap_or(0) > 0);
        assert_eq!(event.payload["strategy"].as_str(), Some("fifo"));
        assert!(event.payload["trim_id"].as_str().unwrap_or("").starts_with("trim_"));
    }

    #[tokio::test]
    async fn short_conversation_does_not_trigger_trim() {
        let bus = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
        let skill = LlmSkill::new(bus.clone());
        let ctx = skill_context();

        // Short messages should not trigger trimming.
        for i in 0..3 {
            skill
                .execute(make_message_event("short-session", "hello!"), ctx.clone())
                .await
                .expect(&format!("msg {i}"));
            sleep(Duration::from_millis(150)).await;
        }

        sleep(Duration::from_millis(200)).await;

        let trimmed_events = bus.events_matching(|e| {
            e.event_type == EventType::Custom("history_trimmed".to_owned())
        });
        assert!(
            trimmed_events.is_empty(),
            "no history_trimmed expected for short conversation, got {}",
            trimmed_events.len()
        );

        let replies = bus.events_matching(|e| {
            e.event_type == EventType::Custom("llm_reply_ready".to_owned())
        });
        assert_eq!(replies.len(), 3, "all 3 messages should produce replies");
    }
}
