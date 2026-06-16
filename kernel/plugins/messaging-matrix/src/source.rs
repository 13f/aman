// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

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

#[allow(dead_code)] // Stub: will be wired into the Matrix sync listener loop.
fn push_chat_event(
    source_id: &str,
    room_id: &str,
    text: &str,
    sender_name: &str,
    sticky_router: &StickyAgentRouter,
    chat_session_store: &ChatSessionStore,
    tx: &mpsc::UnboundedSender<Event>,
) {
    let resolution = sticky_router.resolve(PlatformKind::Matrix, room_id, text);
    let session_id = make_session_id(PlatformKind::Matrix, room_id);

    chat_session_store.store(
        session_id.clone(),
        ChatTarget {
            platform: PlatformKind::Matrix,
            chat_id: room_id.to_owned(),
            source_id: source_id.to_owned(),
            thread_id: None,
        },
    );

    let payload = json!({
        "text": text,
        SESSION_ID_KEY: session_id,
        AGENT_ID_KEY: resolution.agent_id,
        "session_type": "chat",
        "platform": "matrix",
        "source_id": source_id,
        "chat_id": room_id,
        "user_display_name": sender_name,
        CHAT_TARGET_KEY: {
            "platform": "matrix",
            "chat_id": room_id,
            "source_id": source_id,
        }
    });

    let event = Event::new(source_id, EventType::MessageReceived, payload);
    let _ = tx.send(event);
}

#[allow(dead_code)] // Stub: most fields will be used by the Matrix sync listener loop.
pub struct MatrixSource {
    id: String,
    homeserver_url: String,
    username: String,
    password: String,
    device_name: String,
    initialized: bool,
    paused: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    rx: mpsc::UnboundedReceiver<Event>,
    tx: mpsc::UnboundedSender<Event>,
    sticky_router: Option<Arc<StickyAgentRouter>>,
    chat_session_store: Option<Arc<ChatSessionStore>>,
}

impl MatrixSource {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        homeserver_url: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
        device_name: impl Into<String>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            id: id.into(),
            homeserver_url: homeserver_url.into(),
            username: username.into(),
            password: password.into(),
            device_name: device_name.into(),
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
impl EventSource for MatrixSource {
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
            .ok_or_else(|| kernel::Error::config_invalid("MatrixSource missing sticky_router"))?;
        let _chat_session_store = self
            .chat_session_store
            .clone()
            .ok_or_else(|| kernel::Error::config_invalid("MatrixSource missing chat_session_store"))?;

        let source_id = self.id.clone();
        let paused = Arc::clone(&self.paused);
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        self.task = Some(tokio::spawn(async move {
            // Matrix sync loop. Full integration: use matrix-sdk's Client
            // to log in and sync, registering an event handler that calls
            // push_chat_event() for each m.room.message event.
            tracing::info!(%source_id, "matrix source: listener started");

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

            tracing::info!(%source_id, "matrix source: stopped");
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

#[cfg(test)]
mod tests {
    use super::*;
    use messaging_core::router::StickyAgentRouter;
    use messaging_core::session::ChatSessionStore;
    use std::sync::Arc;

    fn registries() -> (Arc<StickyAgentRouter>, Arc<ChatSessionStore>) {
        (
            Arc::new(StickyAgentRouter::new(vec!["cortana".to_owned()])),
            Arc::new(ChatSessionStore::new()),
        )
    }

    #[test]
    fn new_source_has_expected_id_and_type() {
        let source = MatrixSource::new(
            "chat:matrix:test",
            "https://matrix.org",
            "@test:matrix.org",
            "token",
            "aman-agent",
        );
        assert_eq!(source.id(), "chat:matrix:test");
        assert!(matches!(source.source_type(), SourceType::Chat));
        assert!(matches!(source.health(), HealthStatus::Degraded));
    }

    #[tokio::test]
    async fn init_with_registries_sets_health_ok() {
        let (router, store) = registries();
        let mut source = MatrixSource::new(
            "chat:matrix:test",
            "https://matrix.org",
            "@test:matrix.org",
            "token",
            "aman-agent",
        )
        .with_registries(router, store);

        source.init(SourceContext::default()).await.unwrap();
        assert!(matches!(source.health(), HealthStatus::Ok));

        source.shutdown().await.unwrap();
        assert!(matches!(source.health(), HealthStatus::Degraded));
    }

    #[tokio::test]
    async fn init_fails_without_registries() {
        let mut source = MatrixSource::new(
            "chat:matrix:test",
            "https://matrix.org",
            "@test:matrix.org",
            "token",
            "aman-agent",
        );
        let result = source.init(SourceContext::default()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn poll_returns_empty_when_no_events() {
        let (router, store) = registries();
        let mut source = MatrixSource::new(
            "chat:matrix:test",
            "https://matrix.org",
            "@test:matrix.org",
            "token",
            "aman-agent",
        )
        .with_registries(router, store);

        source.init(SourceContext::default()).await.unwrap();
        let events = source.poll(&SourceContext::default()).await.unwrap();
        assert!(events.is_empty());

        source.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pause_resume_and_backpressure() {
        let (router, store) = registries();
        let mut source = MatrixSource::new(
            "chat:matrix:test",
            "https://matrix.org",
            "@test:matrix.org",
            "token",
            "aman-agent",
        )
        .with_registries(Arc::clone(&router), Arc::clone(&store));

        source.init(SourceContext::default()).await.unwrap();

        source.pause().await.unwrap();
        assert!(source.paused.load(Ordering::Acquire));

        source.resume().await.unwrap();
        assert!(!source.paused.load(Ordering::Acquire));

        source.on_backpressure(BackpressureLevel::L3, &SourceContext::default())
            .await
            .unwrap();
        assert!(source.paused.load(Ordering::Acquire));

        source.on_backpressure(BackpressureLevel::Normal, &SourceContext::default())
            .await
            .unwrap();
        assert!(!source.paused.load(Ordering::Acquire));

        source.shutdown().await.unwrap();
    }
}
