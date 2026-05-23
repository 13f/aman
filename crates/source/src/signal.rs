// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::context::SourceContext;
use kernel::event::{Event, EventType};
use kernel::source::EventSource;
use kernel::types::{HealthStatus, SourceType};
use kernel::AmanResult;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub struct SignalSource {
    id: String,
    initialized: bool,
    rx: mpsc::UnboundedReceiver<String>,
    tx: mpsc::UnboundedSender<String>,
    tasks: Vec<JoinHandle<()>>,
}

impl SignalSource {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            id: id.into(),
            initialized: false,
            rx,
            tx,
            tasks: Vec::new(),
        }
    }
}

#[cfg(unix)]
fn spawn_signal_task(kind: tokio::signal::unix::SignalKind, name: &'static str, tx: mpsc::UnboundedSender<String>) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Ok(mut stream) = tokio::signal::unix::signal(kind) {
            loop {
                stream.recv().await;
                let _ = tx.send(name.to_owned());
            }
        }
    })
}

#[async_trait::async_trait]
impl EventSource for SignalSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn source_type(&self) -> SourceType {
        SourceType::Platform
    }

    async fn init(&mut self, _ctx: SourceContext) -> AmanResult<()> {
        if self.initialized {
            return Ok(());
        }

        #[cfg(unix)]
        {
            self.tasks
                .push(spawn_signal_task(tokio::signal::unix::SignalKind::terminate(), "SIGTERM", self.tx.clone()));
            self.tasks
                .push(spawn_signal_task(tokio::signal::unix::SignalKind::interrupt(), "SIGINT", self.tx.clone()));
            self.tasks
                .push(spawn_signal_task(tokio::signal::unix::SignalKind::hangup(), "SIGHUP", self.tx.clone()));
            self.tasks
                .push(spawn_signal_task(tokio::signal::unix::SignalKind::user_defined1(), "SIGUSR1", self.tx.clone()));
        }

        self.initialized = true;
        Ok(())
    }

    async fn shutdown(&mut self) -> AmanResult<()> {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        self.initialized = false;
        Ok(())
    }

    async fn poll(&mut self, _ctx: &SourceContext) -> AmanResult<Vec<Event>> {
        if !self.initialized {
            return Ok(Vec::new());
        }

        let Ok(first) = self.rx.try_recv() else {
            return Ok(Vec::new());
        };
        let mut events = vec![Event::new(
            self.id.clone(),
            EventType::SystemSignal,
            serde_json::json!({ "signal": first }),
        )];

        while let Ok(signal_name) = self.rx.try_recv() {
            events.push(Event::new(
                self.id.clone(),
                EventType::SystemSignal,
                serde_json::json!({ "signal": signal_name }),
            ));
            if events.len() >= 64 {
                break;
            }
        }
        Ok(events)
    }

    fn health(&self) -> HealthStatus {
        if self.initialized {
            HealthStatus::Ok
        } else {
            HealthStatus::Degraded
        }
    }

    async fn reconfigure(&mut self, _config: Value) -> AmanResult<()> {
        if let Some(signal_name) = _config.get("inject_signal").and_then(Value::as_str) {
            let _ = self.tx.send(signal_name.to_owned());
        }
        Ok(())
    }
}

#[cfg(test)]
impl SignalSource {
    pub(crate) fn inject_test_signal(&self, signal_name: &str) {
        let _ = self.tx.send(signal_name.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::SignalSource;
    use kernel::context::{BaseContext, SourceContext};
    use kernel::event::EventType;
    use kernel::source::EventSource;
    use kernel::types::TraceId;

    #[tokio::test]
    async fn emits_system_signal_events_from_queue() {
        let mut source = SignalSource::new("signal:test");
        let ctx = SourceContext {
            base: BaseContext::new(TraceId::new()),
            source_name: Some("signal:test".to_owned()),
        };

        source.init(ctx.clone()).await.expect("init");
        source.inject_test_signal("SIGTERM");
        source.inject_test_signal("SIGINT");

        let events = source.poll(&ctx).await.expect("poll");
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| event.event_type == EventType::SystemSignal));
        source.shutdown().await.expect("shutdown");
    }
}
