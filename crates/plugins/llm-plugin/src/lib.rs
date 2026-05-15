#![forbid(unsafe_code)]
#![doc = "LLM Plugin — processes MESSAGE_RECEIVED events with per-session serial queue."]

use async_trait::async_trait;
use event_bus::{EventBus, EventHandler, SubscriptionFilter, SubscriptionId};
use kernel::context::PluginContext;
use kernel::event::{Event, EventType};
use kernel::plugin::{Plugin, PluginDependency};
use kernel::skill::Skill;
use kernel::source::EventSource;
use kernel::tool::Tool;
use kernel::validator::{OutputValidator, ValidationOutcome};
use kernel::AmanResult;
use serde::Serialize;
use semver::Version;
use serde_json::json;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

const DEFAULT_MAX_QUEUE_PER_SESSION: usize = 10;

/// Configuration for the LLM provider connection.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub provider_key: String,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    /// Base directory for session persistence, e.g. `~/.aman/agents/{key}/sessions`.
    /// When `Some`, every LLM interaction is appended as JSONL:
    ///   `{sessions_dir}/{yyyy-MM}/{yyyy-MM-dd}-{session_id}.jsonl`
    pub sessions_dir: Option<String>,
}

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

/// Event handler that routes MESSAGE_RECEIVED events into per-session queues.
struct LlmEventHandler {
    sessions: Arc<Mutex<HashMap<String, mpsc::Sender<Event>>>>,
    max_queue: usize,
    processed_events: Arc<Mutex<HashSet<String>>>,
    bus: Arc<dyn EventBus>,
    validator: OutputValidator,
    llm_config: LlmConfig,
}

#[async_trait]
impl EventHandler for LlmEventHandler {
    async fn handle(&self, event: Event) -> AmanResult<()> {
        let session_id = match event
            .payload
            .get("session_id")
            .and_then(|v| v.as_str())
        {
            Some(id) => id.to_owned(),
            None => return Ok(()), // skip events without session_id
        };

        let event_id = event.id;

        // UUIDv7 dedup check (§9.2): skip events already processed (WAL replay).
        {
            let mut processed = self.processed_events.lock().expect("processed_events lock");
            if !processed.insert(event_id.to_string()) {
                return Ok(());
            }
        }

        // Get or create session sender under lock.
        let sender = {
            let mut sessions = self.sessions.lock().expect("sessions lock");

            match sessions.entry(session_id.clone()) {
                Entry::Occupied(mut entry) => {
                    if entry.get().is_closed() {
                        let (tx, rx) = mpsc::channel(self.max_queue);
                        spawn_session_processor(rx, self.bus.clone(), session_id.clone(), self.validator.clone(), self.llm_config.clone());
                        entry.insert(tx.clone());
                        tx
                    } else {
                        entry.get().clone()
                    }
                }
                Entry::Vacant(entry) => {
                    let (tx, rx) = mpsc::channel(self.max_queue);
                    spawn_session_processor(rx, self.bus.clone(), session_id.clone(), self.validator.clone(), self.llm_config.clone());
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
                        "plugin:llm",
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
                        "plugin:llm",
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
    llm_config: LlmConfig,
) {
    let mut history = SessionHistory::new();

    while let Some(msg) = rx.recv().await {
        let text = msg
            .payload
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Record user message in history before processing.
        history.push("user", &msg.id.to_string(), text);

        // Persist user message to session JSONL.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        append_session_jsonl(
            llm_config.sessions_dir.as_deref(),
            &session_id,
            &serde_json::json!({
                "role": "user",
                "content": text,
                "event_id": msg.id.to_string(),
                "timestamp": now_ms,
            }),
        );

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
                    "plugin:llm",
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

        // Propagate trace_prev from incoming event to reply (T7.7, §11.6).
        let trace_prev = msg.payload.get("trace_prev").and_then(|v| v.as_str()).map(|s| s.to_owned());

        // Call LLM via rig.
        let reply_text = match call_llm(text, &soul_prompt, &llm_config).await {
            Ok(reply) => reply,
            Err(e) => {
                tracing::error!(%session_id, error = %e, "llm-plugin: LLM call failed");
                // Publish an error event so the frontend can display the failure.
                let error_payload = json!({
                    "session_id": session_id,
                    "original_message_id": msg.id.to_string(),
                    "error": format!("LLM request failed: {e}"),
                    "soul_name": soul_name,
                });
                let error_event = Event::new(
                    "plugin:llm",
                    EventType::Custom("llm_error".to_owned()),
                    error_payload,
                );
                let _ = bus.publish(error_event).await;
                continue;
            }
        };

        // Record assistant reply in history.
        history.push("assistant", &msg.id.to_string(), &reply_text);

        // Persist assistant reply to session JSONL.
        let reply_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        append_session_jsonl(
            llm_config.sessions_dir.as_deref(),
            &session_id,
            &serde_json::json!({
                "role": "assistant",
                "content": reply_text,
                "model": llm_config.model,
                "event_id": msg.id.to_string(),
                "timestamp": reply_ms,
            }),
        );

        // OutputValidator check (§8.2): validate complete reply before publishing.
        let outcome = validator.validate(&reply_text);
        match outcome {
            ValidationOutcome::Pass => {
                let mut reply_payload = json!({
                    "session_id": session_id,
                    "original_message_id": msg.id.to_string(),
                    "reply": reply_text,
                    "soul_name": soul_name,
                });
                if let Some(ref prev) = trace_prev {
                    reply_payload["trace_prev"] = json!(prev);
                    reply_payload["trace_id"] = json!(msg.metadata.trace_id.to_string());
                }
                let reply = Event::new(
                    "plugin:llm",
                    EventType::Custom("llm_reply_ready".to_owned()),
                    reply_payload,
                );
                if let Err(e) = bus.publish(reply).await {
                    tracing::warn!(%session_id, error = %e, "llm-plugin: failed to publish reply");
                }
            }
            ValidationOutcome::Fail { matched_rules, reason } => {
                tracing::warn!(%session_id, matched = ?matched_rules, "llm-plugin: output validation failed");
                let mut blocked_payload = json!({
                    "session_id": session_id,
                    "original_message_id": msg.id.to_string(),
                    "reason": reason,
                    "matched_rules": matched_rules,
                    "soul_name": soul_name,
                });
                if let Some(ref prev) = trace_prev {
                    blocked_payload["trace_prev"] = json!(prev);
                }
                let blocked = Event::new(
                    "plugin:llm",
                    EventType::Custom("output_blocked".to_owned()),
                    blocked_payload,
                );
                let _ = bus.publish(blocked).await;
            }
            ValidationOutcome::Error { message } => {
                tracing::warn!(%session_id, error = %message, "llm-plugin: validator error (fail_closed)");
                let mut blocked_payload = json!({
                    "session_id": session_id,
                    "original_message_id": msg.id.to_string(),
                    "reason": format!("validator_error: {message}"),
                    "fail_closed": true,
                    "soul_name": soul_name,
                });
                if let Some(ref prev) = trace_prev {
                    blocked_payload["trace_prev"] = json!(prev);
                }
                let blocked = Event::new(
                    "plugin:llm",
                    EventType::Custom("output_blocked".to_owned()),
                    blocked_payload,
                );
                let _ = bus.publish(blocked).await;
            }
        }
    }
}

/// Append a JSON line to the session's JSONL file.
///
/// Creates the `{sessions_dir}/{yyyy-MM}/` directory if needed, then
/// appends a JSONL line to `{sessions_dir}/{yyyy-MM}/{yyyy-MM-dd}-{session_id}.jsonl`.
/// Silently returns when `sessions_dir` is `None`.
fn append_session_jsonl(sessions_dir: Option<&str>, session_id: &str, data: &serde_json::Value) {
    let dir = match sessions_dir {
        Some(d) => d,
        None => return,
    };
    let now = chrono::Local::now();
    let month_dir = format!("{}/{}", dir, now.format("%Y-%m"));
    let file_path = format!("{}/{}-{}.jsonl", month_dir, now.format("%Y-%m-%d"), session_id);
    if let Err(e) = std::fs::create_dir_all(&month_dir) {
        tracing::warn!(error = %e, "llm-plugin: failed to create sessions month dir");
        return;
    }
    let line = match serde_json::to_string(data) {
        Ok(s) => s + "\n",
        Err(e) => {
            tracing::warn!(error = %e, "llm-plugin: failed to serialize session line");
            return;
        }
    };
    if let Err(e) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
        .and_then(|f| {
            use std::io::Write;
            let mut writer = std::io::BufWriter::new(f);
            writer.write_all(line.as_bytes())
        })
    {
        tracing::warn!(error = %e, "llm-plugin: failed to write session line");
    }
}

/// Call the LLM via rig's OpenAI-compatible client.
async fn call_llm(text: &str, soul_prompt: &str, config: &LlmConfig) -> Result<String, String> {
    use rig_core::client::CompletionClient;
    use rig_core::completion::Prompt;

    let client = rig_core::providers::openai::CompletionsClient::builder()
        .api_key(&config.api_key)
        .base_url(&config.base_url)
        .build()
        .map_err(|e| format!("failed to build rig client: {e}"))?;

    let mut agent_builder = client.agent(&config.model);
    if !soul_prompt.is_empty() {
        agent_builder = agent_builder.preamble(soul_prompt);
    }

    let agent = agent_builder.build();
    agent
        .prompt(text)
        .await
        .map_err(|e| format!("LLM prompt failed: {e}"))
}

fn spawn_session_processor(
    rx: mpsc::Receiver<Event>,
    bus: Arc<dyn EventBus>,
    session_id: String,
    validator: OutputValidator,
    llm_config: LlmConfig,
) {
    tokio::spawn(async move {
        process_session(rx, bus, session_id, validator, llm_config).await;
    });
}

/// LLM Plugin — subscribes to MESSAGE_RECEIVED events and processes them
/// with per-session serial queues.
///
/// Loaded via PluginLoader during startup. Subscribes to the EventBus
/// directly in `on_load()`, bypassing the Skill dispatch system entirely.
pub struct LlmPlugin {
    name: String,
    version: Version,
    bus: Arc<dyn EventBus>,
    sessions: Arc<Mutex<HashMap<String, mpsc::Sender<Event>>>>,
    max_queue: usize,
    processed_events: Arc<Mutex<HashSet<String>>>,
    validator: OutputValidator,
    llm_config: LlmConfig,
    subscription_id: Mutex<Option<SubscriptionId>>,
}

impl LlmPlugin {
    #[must_use]
    pub fn new(bus: Arc<dyn EventBus>, llm_config: LlmConfig) -> Self {
        Self {
            name: "llm-plugin".to_owned(),
            version: Version::new(0, 1, 0),
            bus,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            max_queue: DEFAULT_MAX_QUEUE_PER_SESSION,
            processed_events: Arc::new(Mutex::new(HashSet::new())),
            validator: OutputValidator::new(),
            llm_config,
            subscription_id: Mutex::new(None),
        }
    }

    /// Number of currently active sessions.
    #[must_use]
    pub fn active_session_count(&self) -> usize {
        self.sessions.lock().expect("sessions lock").len()
    }

    /// Drain all active sessions: close every per-session channel.
    pub fn drain_sessions(&self) -> usize {
        let mut sessions = self.sessions.lock().expect("sessions lock");
        let count = sessions.len();
        sessions.clear();
        count
    }
}

#[async_trait]
impl Plugin for LlmPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &Version {
        &self.version
    }

    fn dependencies(&self) -> &[PluginDependency] {
        &[]
    }

    async fn on_load(&mut self, _ctx: PluginContext) -> AmanResult<()> {
        let handler = Box::new(LlmEventHandler {
            sessions: Arc::clone(&self.sessions),
            max_queue: self.max_queue,
            processed_events: Arc::clone(&self.processed_events),
            bus: Arc::clone(&self.bus),
            validator: self.validator.clone(),
            llm_config: self.llm_config.clone(),
        });
        let filter = SubscriptionFilter {
            event_types: Some(vec![EventType::MessageReceived]),
            ..SubscriptionFilter::default()
        };
        let id = self.bus.subscribe(filter, handler).await?;
        *self.subscription_id.lock().expect("subscription_id lock") = Some(id);
        tracing::info!("llm-plugin: subscribed to MESSAGE_RECEIVED events");
        Ok(())
    }

    async fn on_unload(&mut self) -> AmanResult<()> {
        // Unsubscribe from bus first (drop the lock before the await)
        let sub_id = {
            self.subscription_id.lock().expect("subscription_id lock").take()
        };
        if let Some(id) = sub_id {
            self.bus.unsubscribe(id).await;
        }
        // Then drain remaining sessions
        let drained = self.drain_sessions();
        tracing::info!(drained, "llm-plugin: unloaded, sessions drained");
        Ok(())
    }

    async fn on_dependency_unloading(&self, _dep_name: &str) -> AmanResult<()> {
        Ok(())
    }

    fn event_sources(&self) -> Vec<Arc<dyn EventSource>> {
        vec![]
    }

    fn skills(&self) -> Vec<Arc<dyn Skill>> {
        vec![]
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_utils::fake_event_bus::{FakeBusConfig, FakeEventBus};
    use tokio::time::{sleep, Duration};

    fn test_llm_config() -> LlmConfig {
        LlmConfig {
            provider_key: "test".to_owned(),
            api_key: "test-key".to_owned(),
            base_url: "http://localhost:99999/nonexistent".to_owned(),
            model: "test-model".to_owned(),
            sessions_dir: None,
        }
    }

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

    async fn load_plugin(bus: Arc<FakeEventBus>, config: LlmConfig) -> LlmPlugin {
        let mut plugin = LlmPlugin::new(bus, config);
        plugin
            .on_load(PluginContext::default())
            .await
            .expect("plugin on_load");
        plugin
    }

    #[tokio::test]
    async fn accepts_message_received_event() {
        let bus = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
        let _plugin = load_plugin(bus.clone(), test_llm_config()).await;
        bus.publish(make_message_event("session-1", "hello"))
            .await
            .expect("publish");

        // Should have published message_queued via the handler
        let queued = bus.events_matching(|e| {
            e.event_type == EventType::Custom("message_queued".to_owned())
        });
        assert_eq!(queued.len(), 1, "should publish message_queued");
        assert_eq!(
            queued[0].payload["session_id"].as_str(),
            Some("session-1")
        );

        // Should eventually produce an LLM error (no real endpoint in tests)
        sleep(Duration::from_millis(200)).await;
        let errors = bus.events_matching(|e| {
            e.event_type == EventType::Custom("llm_error".to_owned())
        });
        assert_eq!(errors.len(), 1, "should produce one LLM error");
    }

    #[tokio::test]
    async fn drops_message_when_session_queue_is_full() {
        let bus = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
        let mut plugin = LlmPlugin::new(bus.clone(), test_llm_config());
        // Use a minimal max_queue size and make the test bus dispatch inline
        // by using the inner bus's direct publish mechanism.
        //
        // We set a short queue and rely on the background session processor
        // being occupied with the first message.
        plugin.max_queue = 1;
        plugin
            .on_load(PluginContext::default())
            .await
            .expect("plugin on_load");

        // First message starts processing (background task picks it up).
        bus.publish(make_message_event("busy-session", "first"))
            .await
            .expect("publish first");

        // Wait so the background task picks up the first message, freeing the mpsc slot.
        sleep(Duration::from_millis(20)).await;

        // Second message fills the channel slot.
        bus.publish(make_message_event("busy-session", "second"))
            .await
            .expect("publish second");

        // Third message should be dropped (queue full).
        bus.publish(make_message_event("busy-session", "third"))
            .await
            .expect("publish third");

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
        let _plugin = load_plugin(bus.clone(), test_llm_config()).await;

        // Send two messages for the same session.
        bus.publish(make_message_event("seq-session", "first"))
            .await
            .expect("publish first");
        bus.publish(make_message_event("seq-session", "second"))
            .await
            .expect("publish second");

        // Wait for both to be processed (2 * 100ms + margin).
        sleep(Duration::from_millis(300)).await;

        let errors = bus.events_matching(|e| {
            e.event_type == EventType::Custom("llm_error".to_owned())
        });
        assert_eq!(errors.len(), 2, "both messages should produce LLM errors");
    }

    #[tokio::test]
    async fn different_sessions_processed_independently() {
        let bus = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
        let _plugin = load_plugin(bus.clone(), test_llm_config()).await;

        bus.publish(make_message_event("session-a", "from a"))
            .await
            .expect("publish a");
        bus.publish(make_message_event("session-b", "from b"))
            .await
            .expect("publish b");

        sleep(Duration::from_millis(200)).await;

        let errors = bus.events_matching(|e| {
            e.event_type == EventType::Custom("llm_error".to_owned())
        });
        assert_eq!(errors.len(), 2);
    }

    #[tokio::test]
    async fn plugin_metadata() {
        let bus = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
        let plugin = LlmPlugin::new(bus, test_llm_config());
        assert_eq!(plugin.name(), "llm-plugin");
        assert_eq!(plugin.version(), &Version::new(0, 1, 0));
        assert_eq!(plugin.active_session_count(), 0);
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
        let _plugin = load_plugin(bus.clone(), test_llm_config()).await;
        let long_text = "X".repeat(4096); // ≈1024 tokens per message

        // Send enough messages to trigger trimming.
        for i in 0..6 {
            bus.publish(make_message_event("trim-session", &long_text))
                .await
                .expect(&format!("message {i} accepted"));
            // Wait for each message to be processed before sending the next,
            // so each becomes a full pair in history.
            sleep(Duration::from_millis(150)).await;
        }

        // Now send one more message to trigger trim.
        sleep(Duration::from_millis(200)).await;
        bus.publish(make_message_event("trim-session", &long_text))
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
        let _plugin = load_plugin(bus.clone(), test_llm_config()).await;

        // Short messages should not trigger trimming.
        for i in 0..3 {
            bus.publish(make_message_event("short-session", "hello!"))
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

        let errors = bus.events_matching(|e| {
            e.event_type == EventType::Custom("llm_error".to_owned())
        });
        assert_eq!(errors.len(), 3, "all 3 messages should produce LLM errors");
    }
}
