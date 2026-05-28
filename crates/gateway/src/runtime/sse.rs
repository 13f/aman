#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::StreamExt as _;
use tracing::error;

use crate::runtime::AgentRuntime;

/// A message pushed onto the SSE broadcast channel.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SseMessage {
    /// Tauri event name, e.g. "event:processed", "metrics:updated", etc.
    pub event_type: String,
    /// JSON payload — shape must match exactly what the frontend expects.
    pub payload: serde_json::Value,
}

/// Shared state for the SSE route handler.
///
/// The inner `broadcast::Sender` fans out to all connected SSE clients.
pub(crate) struct SseBroadcastState {
    tx: broadcast::Sender<SseMessage>,
}

impl SseBroadcastState {
    /// Subscribe to the broadcast stream. Each SSE client gets its own receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<SseMessage> {
        self.tx.subscribe()
    }
}

/// Create an empty SSE broadcast state (no background tasks yet).
pub(crate) fn new_sse_state() -> Arc<SseBroadcastState> {
    let (tx, _rx) = broadcast::channel::<SseMessage>(256);
    Arc::new(SseBroadcastState { tx })
}

/// Start the SSE background tasks.
///
/// Two tasks are spawned:
///  - **Task A**: subscribes to the global EventBus and forwards every event
///    as an `event:processed` SSE message.
///  - **Task B**: every 2s, emits snapshot messages for metrics, runtime status,
///    agent states, and notification unread count.
pub(crate) fn start_sse_tasks(runtime: &Arc<AgentRuntime>) {
    let tx = runtime.sse_broadcast().tx.clone();

    // Task A — EventBus subscription
    let bus_tx = tx.clone();
    let bus = runtime.bus_cloned();
    tokio::spawn(async move {
        let handler = Box::new(SseBusHandler { tx: bus_tx });
        match bus
            .subscribe(event_bus::SubscriptionFilter::default(), handler)
            .await
        {
            Ok(_sub_id) => {
                // Keep the subscription alive forever.
                std::future::pending::<()>().await;
            }
            Err(e) => {
                error!(error = %e, "sse: failed to subscribe to EventBus");
            }
        }
    });

    // Task B — periodic snapshots
    let snapshot_tx = tx;
    let snapshot_runtime = Arc::clone(runtime);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            emit_snapshots(&snapshot_runtime, &snapshot_tx).await;
        }
    });
}

// ── EventBus handler ──────────────────────────────────────────────────────

struct SseBusHandler {
    tx: broadcast::Sender<SseMessage>,
}

#[async_trait::async_trait]
impl event_bus::EventHandler for SseBusHandler {
    async fn handle(&self, event: kernel::event::Event) -> kernel::AmanResult<()> {
        let payload = serde_json::to_value(&event).unwrap_or_default();
        let msg = SseMessage {
            event_type: "event:processed".to_owned(),
            payload,
        };
        let _ = self.tx.send(msg);
        Ok(())
    }
}

// ── Snapshot emitters ─────────────────────────────────────────────────────

async fn emit_snapshots(runtime: &AgentRuntime, tx: &broadcast::Sender<SseMessage>) {
    // runtime:updated
    let rt = serde_json::json!({
        "phase": runtime.phase() as u8,
        "ready": runtime.is_ready(),
        "live": runtime.is_live(),
    });
    let _ = tx.send(SseMessage {
        event_type: "runtime:updated".into(),
        payload: rt,
    });

    // metrics:updated
    let metrics = metrics_snapshot(runtime).await;
    let _ = tx.send(SseMessage {
        event_type: "metrics:updated".into(),
        payload: metrics,
    });

    // agent_states:updated
    let agents = agent_states_snapshot(runtime).await;
    let _ = tx.send(SseMessage {
        event_type: "agent_states:updated".into(),
        payload: agents,
    });

    // notification:updated
    let notif = serde_json::json!({
        "unread_count": runtime.notifications().unread_count(),
    });
    let _ = tx.send(SseMessage {
        event_type: "notification:updated".into(),
        payload: notif,
    });
}

async fn metrics_snapshot(runtime: &AgentRuntime) -> serde_json::Value {
    let bus = runtime.bus_metrics();
    let plugin_health = {
        let loader = runtime.plugin_loader().await;
        let mut entries: Vec<serde_json::Value> = Vec::new();
        for name in loader.loaded_plugins() {
            let status = match loader.state_of(&name) {
                Some(s) => format!("{s:?}"),
                None => "unknown".to_owned(),
            };
            entries.push(serde_json::json!({ "name": name, "status": status }));
        }
        entries
    };

    serde_json::json!({
        "queue_depth": {
            "high": bus.queue_depth.high,
            "normal": bus.queue_depth.normal,
            "low": bus.queue_depth.low,
        },
        "throughput": bus.throughput,
        "discarded": bus.discarded_count,
        "duplicate": bus.duplicate_count,
        "subscription_count": bus.subscription_count,
        "retry_queue_depth": bus.retry_queue_depth,
        "dlq_depth": runtime.dlq().depth(),
        "inflight_pipelines": runtime.inflight_pipelines(),
        "inflight_skills": runtime.inflight_skills(),
        "backpressure_level": format!("{:?}", bus.backpressure_level),
        "plugin_health": plugin_health,
    })
}

async fn agent_states_snapshot(runtime: &AgentRuntime) -> serde_json::Value {
    let instances = runtime.agent_registry().list().await;
    let agents: Vec<serde_json::Value> = instances
        .into_iter()
        .map(|inst| {
            serde_json::json!({
                "agent_id": inst.descriptor.agent_id,
                "system_state": serde_json::to_value(inst.system_state)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| "idle".to_owned()),
                "status": serde_json::to_value(inst.status)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
            })
        })
        .collect();
    serde_json::json!({ "agents": agents })
}

// ── Axum route handler ────────────────────────────────────────────────────

pub(crate) async fn sse_stream_handler(
    state: axum::extract::State<Arc<AgentRuntime>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.sse_broadcast().subscribe();

    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(msg) => {
            let data = serde_json::to_string(&msg.payload).unwrap_or_default();
            Some(Ok(Event::default()
                .event(msg.event_type)
                .data(data)))
        }
        Err(BroadcastStreamRecvError::Lagged(skipped)) => {
            tracing::warn!(skipped, "sse: client lagging, dropped messages");
            None
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}
