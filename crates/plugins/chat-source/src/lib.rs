//! Chat platform source — a push-based EventSource that validates chat messages
//! and delivers them as `MESSAGE_RECEIVED` events via an mpsc channel.
//!
//! # Channel types
//!
//! - `tauri_desktop` — messages arrive via Tauri IPC (`chat:send_message`)
//! - Future: `websocket`, `cli`
//!
//! # Architecture
//!
//! Follows the same push-based pattern as `WebhookSource`: callers push messages
//! through a channel sender, and `poll()` returns them to the `SourceRegistry`
//! which publishes to the `EventBus`.

#![forbid(unsafe_code)]

use async_trait::async_trait;
use kernel::context::SourceContext;
use kernel::event::{Event, EventType};
use kernel::source::EventSource;
use kernel::types::{BackpressureLevel, HealthStatus, SourceType};
use kernel::AmanResult;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::instrument;
use uuid::Uuid;

/// Default maximum message length in Unicode code points.
pub const DEFAULT_MAX_MESSAGE_LENGTH: usize = 4096;

/// Channel type through which the message arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelType {
    TauriDesktop,
}

impl ChannelType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::TauriDesktop => "tauri_desktop",
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ValidationError {
    #[error("message exceeds maximum length of {max} characters (got {actual})")]
    TooLong { max: usize, actual: usize },
    #[error("message is empty")]
    Empty,
    #[error("session_id is empty")]
    MissingSession,
}

/// A push-based EventSource for chat messages.
///
/// Callers push validated messages through [`ChatPlatformSource::handle_message`],
/// and the `SourceRegistry` picks them up via `poll()` to publish to the EventBus.
pub struct ChatPlatformSource {
    id: String,
    channel_type: ChannelType,
    max_message_length: usize,
    initialized: bool,
    paused: Arc<AtomicBool>,
    tx: mpsc::UnboundedSender<Event>,
    rx: mpsc::UnboundedReceiver<Event>,
}

impl ChatPlatformSource {
    /// Create a new source for the Tauri desktop channel.
    pub fn new_tauri_desktop() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            id: "chat-platform:tauri-desktop".to_owned(),
            channel_type: ChannelType::TauriDesktop,
            max_message_length: DEFAULT_MAX_MESSAGE_LENGTH,
            initialized: false,
            paused: Arc::new(AtomicBool::new(false)),
            tx,
            rx,
        }
    }

    /// Override the maximum allowed message length.
    pub fn with_max_message_length(mut self, max: usize) -> Self {
        self.max_message_length = max;
        self
    }

    /// Get a sender handle that can be used to push messages from outside (e.g. Tauri commands).
    ///
    /// This allows the Tauri command handler to call `handle_message` through
    /// an `Arc<ChatPlatformSource>` without needing mutable access.
    pub fn sender(&self) -> mpsc::UnboundedSender<Event> {
        self.tx.clone()
    }

    /// Validate a chat message.
    pub fn validate_message(&self, text: &str, session_id: &str) -> Result<(), ValidationError> {
        if session_id.is_empty() {
            return Err(ValidationError::MissingSession);
        }
        if text.is_empty() {
            return Err(ValidationError::Empty);
        }
        let len = text.chars().count();
        if len > self.max_message_length {
            return Err(ValidationError::TooLong {
                max: self.max_message_length,
                actual: len,
            });
        }
        Ok(())
    }

    /// Validate and push a chat message into the event channel.
    ///
    /// Returns `Ok(())` if the event was enqueued, or a validation error.
    /// The event will be picked up by `poll()` and published by the `SourceRegistry`.
    #[instrument(skip(self), fields(session_id, text_len = text.chars().count()))]
    pub fn handle_message(
        &self,
        text: &str,
        session_id: &str,
    ) -> Result<(), ValidationError> {
        self.validate_message(text, session_id)?;
        let event = self.create_event(text, session_id);
        let _ = self.tx.send(event);
        Ok(())
    }

    /// Create a `MESSAGE_RECEIVED` Event from a chat message (without validation).
    pub fn create_event(&self, text: &str, session_id: &str) -> Event {
        let payload = serde_json::json!({
            "session_id": session_id,
            "text": text,
            "channel": self.channel_type.as_str(),
            "message_id": Uuid::now_v7(),
            "client_timestamp": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
        });
        Event::new(self.id.clone(), EventType::MessageReceived, payload)
    }
}

#[async_trait]
impl EventSource for ChatPlatformSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn source_type(&self) -> SourceType {
        SourceType::Platform
    }

    async fn init(&mut self, _ctx: SourceContext) -> AmanResult<()> {
        self.initialized = true;
        self.paused.store(false, Ordering::Release);
        tracing::info!(id = %self.id, "ChatPlatformSource initialized");
        Ok(())
    }

    async fn shutdown(&mut self) -> AmanResult<()> {
        self.paused.store(true, Ordering::Release);
        self.initialized = false;
        tracing::info!(id = %self.id, "ChatPlatformSource shut down");
        Ok(())
    }

    async fn poll(&mut self, _ctx: &SourceContext) -> AmanResult<Vec<Event>> {
        if !self.initialized || self.paused.load(Ordering::Acquire) {
            return Ok(Vec::new());
        }

        let mut events = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            events.push(event);
            if events.len() >= 256 {
                break;
            }
        }
        Ok(events)
    }

    async fn on_backpressure(&mut self, level: BackpressureLevel, _ctx: &SourceContext) -> AmanResult<()> {
        let should_pause = matches!(
            level,
            BackpressureLevel::L3 | BackpressureLevel::L4A | BackpressureLevel::L4B | BackpressureLevel::Critical
        );
        self.paused.store(should_pause, Ordering::Release);
        Ok(())
    }

    fn health(&self) -> HealthStatus {
        if self.initialized {
            HealthStatus::Ok
        } else {
            HealthStatus::Degraded
        }
    }

    async fn pause(&mut self) -> AmanResult<()> {
        self.paused.store(true, Ordering::Release);
        Ok(())
    }

    async fn resume(&mut self) -> AmanResult<()> {
        if self.initialized {
            self.paused.store(false, Ordering::Release);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_id_and_type() {
        let source = ChatPlatformSource::new_tauri_desktop();
        assert_eq!(source.id(), "chat-platform:tauri-desktop");
        assert_eq!(source.source_type(), SourceType::Platform);
    }

    #[test]
    fn validates_messages() {
        let source = ChatPlatformSource::new_tauri_desktop();
        assert!(source.validate_message("hello", "s1").is_ok());
        assert!(source.validate_message("", "s1").is_err());
        assert!(source.validate_message("text", "").is_err());
    }

    #[test]
    fn rejects_overly_long_message() {
        let source = ChatPlatformSource::new_tauri_desktop().with_max_message_length(5);
        assert!(source.validate_message("12345", "s1").is_ok());
        assert!(source.validate_message("123456", "s1").is_err());
    }

    #[test]
    fn create_event_has_correct_structure() {
        let source = ChatPlatformSource::new_tauri_desktop();
        let event = source.create_event("你好", "session-1");
        assert_eq!(event.event_type, EventType::MessageReceived);
        assert_eq!(event.source.as_str(), "chat-platform:tauri-desktop");
        assert_eq!(event.payload["session_id"], "session-1");
        assert_eq!(event.payload["text"], "你好");
        assert_eq!(event.payload["channel"], "tauri_desktop");
        assert!(event.payload["message_id"].is_string());
        assert!(event.payload["client_timestamp"].is_number());
    }

    #[tokio::test]
    async fn handle_message_enqueues_event() {
        let mut source = ChatPlatformSource::new_tauri_desktop();
        let ctx = SourceContext {
            base: kernel::context::BaseContext::new(kernel::types::TraceId::new()),
            source_name: Some(source.id().to_owned()),
        };
        source.init(ctx.clone()).await.unwrap();

        source.handle_message("hello", "s1").unwrap();

        // poll should pick it up
        let events = source.poll(&ctx).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, EventType::MessageReceived);
        assert_eq!(events[0].payload["text"], "hello");
    }

    #[test]
    fn handle_message_validates_before_enqueuing() {
        let source = ChatPlatformSource::new_tauri_desktop();
        assert!(source.handle_message("", "s1").is_err());
        assert!(source.handle_message("text", "").is_err());
    }

    #[tokio::test]
    async fn does_not_poll_when_paused() {
        let mut source = ChatPlatformSource::new_tauri_desktop();
        let ctx = SourceContext {
            base: kernel::context::BaseContext::new(kernel::types::TraceId::new()),
            source_name: Some(source.id().to_owned()),
        };
        source.init(ctx.clone()).await.unwrap();
        source.pause().await.unwrap();
        source.handle_message("hello", "s1").unwrap();
        let events = source.poll(&ctx).await.unwrap();
        assert!(events.is_empty(), "should not return events while paused");

        source.resume().await.unwrap();
        // After resume, events that were queued while paused should now be available
        let events = source.poll(&ctx).await.unwrap();
        assert!(!events.is_empty(), "should return queued events after resume");
    }

    #[tokio::test]
    async fn sender_clone_works() {
        let source = ChatPlatformSource::new_tauri_desktop();
        let sender = source.sender();

        let event = Event::new("external", EventType::MessageReceived, serde_json::json!({"text": "external"}));
        sender.send(event).unwrap();

        let ctx = SourceContext {
            base: kernel::context::BaseContext::new(kernel::types::TraceId::new()),
            source_name: Some(source.id().to_owned()),
        };
        let mut source = source;
        source.init(ctx.clone()).await.unwrap();

        // External event should be picked up by poll
        let events = source.poll(&ctx).await.unwrap();
        assert!(!events.is_empty());
        assert_eq!(events[0].payload["text"], "external");
    }
}
