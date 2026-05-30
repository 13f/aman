// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! [`SlackSource`] — `EventSource` for Slack Socket Mode / Events API.

use async_trait::async_trait;
use kernel::context::SourceContext;
use kernel::event::{Event, EventType};
use kernel::source::EventSource;
use kernel::types::{BackpressureLevel, HealthStatus, SourceType};
use kernel::AmanResult;
use messaging_core::router::StickyAgentRouter;
use messaging_core::session::ChatSessionStore;
use messaging_core::types::{
    make_session_id, ChatTarget, PlatformKind, AGENT_ID_KEY, CHAT_TARGET_KEY, SESSION_ID_KEY,
};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Duration;

const POLL_TIMEOUT_MS: u64 = 25;
const MAX_BATCH_SIZE: usize = 256;

/// Build a chat event from an incoming Slack message.
fn push_chat_event(
    source_id: &str,
    chat_id: &str,
    text: &str,
    user_display_name: Option<&str>,
    thread_id: Option<&str>,
    sticky_router: &StickyAgentRouter,
    chat_session_store: &ChatSessionStore,
    tx: &mpsc::UnboundedSender<Event>,
) {
    let resolution = sticky_router.resolve(PlatformKind::Slack, chat_id, text);
    let session_id = make_session_id(PlatformKind::Slack, chat_id);

    chat_session_store.store(
        session_id.clone(),
        ChatTarget {
            platform: PlatformKind::Slack,
            chat_id: chat_id.to_owned(),
            source_id: source_id.to_owned(),
            thread_id: thread_id.map(str::to_owned),
        },
    );

    let payload = json!({
        "text": text,
        SESSION_ID_KEY: session_id,
        AGENT_ID_KEY: resolution.agent_id,
        "platform": "slack",
        "source_id": source_id,
        "chat_id": chat_id,
        "thread_id": thread_id,
        "user_display_name": user_display_name,
        CHAT_TARGET_KEY: {
            "platform": "slack",
            "chat_id": chat_id,
            "source_id": source_id,
            "thread_id": thread_id,
        }
    });

    let event = Event::new(source_id, EventType::MessageReceived, payload);
    let _ = tx.send(event);
}

pub struct SlackSource {
    id: String,
    bot_token: String,
    app_token: String,
    initialized: bool,
    paused: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    rx: mpsc::UnboundedReceiver<Event>,
    tx: mpsc::UnboundedSender<Event>,
    sticky_router: Option<Arc<StickyAgentRouter>>,
    chat_session_store: Option<Arc<ChatSessionStore>>,
}

impl SlackSource {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        bot_token: impl Into<String>,
        app_token: impl Into<String>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            id: id.into(),
            bot_token: bot_token.into(),
            app_token: app_token.into(),
            initialized: false,
            paused: Arc::new(AtomicBool::new(false)),
            task: None,
            shutdown_tx: None,
            rx,
            tx,
            sticky_router: None,
            chat_session_store: None,
        }
    }

    #[must_use]
    pub fn with_registries(
        mut self,
        sticky_router: Arc<StickyAgentRouter>,
        chat_session_store: Arc<ChatSessionStore>,
    ) -> Self {
        self.sticky_router = Some(sticky_router);
        self.chat_session_store = Some(chat_session_store);
        self
    }
}

#[async_trait]
impl EventSource for SlackSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn source_type(&self) -> SourceType {
        SourceType::Chat
    }

    async fn init(&mut self, _ctx: SourceContext) -> AmanResult<()> {
        if self.initialized {
            return Ok(());
        }

        let _sticky_router = self
            .sticky_router
            .clone()
            .ok_or_else(|| kernel::Error::config_invalid("SlackSource missing sticky_router"))?;
        let _chat_session_store = self
            .chat_session_store
            .clone()
            .ok_or_else(|| kernel::Error::config_invalid("SlackSource missing chat_session_store"))?;

        let source_id = self.id.clone();
        let paused = Arc::clone(&self.paused);
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        self.task = Some(tokio::spawn(async move {
            // Slack Socket Mode / Events API listener loop.
            // Full integration: use slack-morphism's SlackClient with socket_mode
            // to receive events, then call push_chat_event() for each message.
            tracing::info!(%source_id, "slack source: listener started");

            loop {
                if shutdown_rx.try_recv().is_ok() {
                    break;
                }
                if paused.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }

            tracing::info!(%source_id, "slack source: stopped");
        }));

        self.paused.store(false, Ordering::Release);
        self.initialized = true;
        Ok(())
    }

    async fn shutdown(&mut self) -> AmanResult<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.paused.store(true, Ordering::Release);
        self.initialized = false;
        Ok(())
    }

    async fn poll(&mut self, _ctx: &SourceContext) -> AmanResult<Vec<Event>> {
        if !self.initialized {
            return Ok(Vec::new());
        }
        match tokio::time::timeout(Duration::from_millis(POLL_TIMEOUT_MS), self.rx.recv()).await {
            Ok(Some(first)) => {
                let mut events = vec![first];
                while let Ok(event) = self.rx.try_recv() {
                    events.push(event);
                    if events.len() >= MAX_BATCH_SIZE {
                        break;
                    }
                }
                Ok(events)
            }
            Ok(None) | Err(_) => Ok(Vec::new()),
        }
    }

    async fn on_backpressure(
        &mut self,
        level: BackpressureLevel,
        _ctx: &SourceContext,
    ) -> AmanResult<()> {
        let should_pause = matches!(
            level,
            BackpressureLevel::L3
                | BackpressureLevel::L4A
                | BackpressureLevel::L4B
                | BackpressureLevel::Critical
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
