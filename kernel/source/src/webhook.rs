// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use kernel::context::SourceContext;
use kernel::event::{Event, EventType};
use kernel::source::EventSource;
use kernel::types::{BackpressureLevel, HealthStatus, SourceType};
use kernel::AmanResult;
use serde_json::Value;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Duration;

const WEBHOOK_POLL_TIMEOUT_MS: u64 = 25;

struct WebhookState {
    source_id: String,
    paused: Arc<AtomicBool>,
    tx: mpsc::UnboundedSender<Event>,
}

async fn ingest_handler(
    State(state): State<Arc<WebhookState>>,
    Json(payload): Json<Value>,
) -> impl IntoResponse {
    if state.paused.load(Ordering::Acquire) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }

    let event = Event::new(state.source_id.clone(), EventType::WebhookReceived, payload);
    if state.tx.send(event).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    StatusCode::ACCEPTED.into_response()
}

pub struct WebhookSource {
    id: String,
    path: String,
    port: u16,
    initialized: bool,
    paused: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    rx: mpsc::UnboundedReceiver<Event>,
    tx: mpsc::UnboundedSender<Event>,
}

impl WebhookSource {
    #[must_use]
    pub fn new(id: impl Into<String>, path: impl Into<String>, port: u16) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            id: id.into(),
            path: path.into(),
            port,
            initialized: false,
            paused: Arc::new(AtomicBool::new(false)),
            task: None,
            shutdown_tx: None,
            rx,
            tx,
        }
    }

    fn build_router(&self) -> Router {
        let state = Arc::new(WebhookState {
            source_id: self.id.clone(),
            paused: Arc::clone(&self.paused),
            tx: self.tx.clone(),
        });
        Router::new()
            .route(self.path.as_str(), post(ingest_handler))
            .with_state(state)
    }
}

#[async_trait::async_trait]
impl EventSource for WebhookSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn source_type(&self) -> SourceType {
        SourceType::Webhook
    }

    async fn init(&mut self, _ctx: SourceContext) -> AmanResult<()> {
        if self.initialized {
            return Ok(());
        }

        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), self.port);
        let listener = tokio::net::TcpListener::bind(addr).await?;
        self.port = listener.local_addr()?.port();

        let app = self.build_router();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        self.task = Some(tokio::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            });
            let _ = server.await;
        }));
        self.paused.store(false, Ordering::Release);
        self.initialized = true;
        Ok(())
    }

    async fn shutdown(&mut self) -> AmanResult<()> {
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(());
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

        match tokio::time::timeout(Duration::from_millis(WEBHOOK_POLL_TIMEOUT_MS), self.rx.recv())
            .await
        {
            Ok(Some(first_event)) => {
                let mut events = vec![first_event];
                while let Ok(event) = self.rx.try_recv() {
                    events.push(event);
                    if events.len() >= 256 {
                        break;
                    }
                }
                Ok(events)
            }
            Ok(None) => Ok(Vec::new()),
            Err(_) => Ok(Vec::new()),
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

    async fn reconfigure(&mut self, config: Value) -> AmanResult<()> {
        if let Some(path) = config.get("path").and_then(Value::as_str) {
            self.path = path.to_owned();
        }
        if let Some(port) = config.get("port").and_then(Value::as_u64) {
            self.port = u16::try_from(port).unwrap_or(self.port);
        }
        Ok(())
    }
}
