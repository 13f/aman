// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use event_bus::EventBus;
use kernel::context::{BaseContext, SourceContext};
use kernel::event::Event;
use kernel::source::EventSource;
use kernel::types::{BackpressureLevel, HealthStatus, SourceType, TraceId};
use kernel::{AmanResult, Error};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{sleep, Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceMode {
    Pull,
    Push,
}

// TrustLevel is now defined in kernel::types for universal access.
// Re-export for backward compatibility.
pub use kernel::types::TrustLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceLifecycleState {
    Registered,
    Running,
    Paused,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSnapshot {
    pub id: String,
    pub source_type: SourceType,
    pub mode: SourceMode,
    pub trust_level: TrustLevel,
    pub state: SourceLifecycleState,
    pub health: HealthStatus,
}

struct RegisteredSource {
    id: String,
    source_type: SourceType,
    mode: SourceMode,
    trust_level: TrustLevel,
    state: Arc<RwLock<SourceLifecycleState>>,
    source: Arc<Mutex<Box<dyn EventSource>>>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
    paused_by_backpressure: AtomicBool,
}

impl RegisteredSource {
    async fn snapshot(&self) -> SourceSnapshot {
        let state = *self.state.read().await;
        let health = {
            let source = self.source.lock().await;
            source.health()
        };
        SourceSnapshot {
            id: self.id.clone(),
            source_type: self.source_type,
            mode: self.mode,
            trust_level: self.trust_level,
            state,
            health,
        }
    }
}

pub struct SourceRegistry {
    bus: Arc<dyn EventBus>,
    sources: RwLock<HashMap<String, Arc<RegisteredSource>>>,
}

impl SourceRegistry {
    #[must_use]
    pub fn new(bus: Arc<dyn EventBus>) -> Self {
        Self {
            bus,
            sources: RwLock::new(HashMap::new()),
        }
    }

    pub async fn register(
        &self,
        source: Box<dyn EventSource>,
        mode: SourceMode,
        trust_level: TrustLevel,
    ) -> AmanResult<()> {
        let id = source.id().to_owned();
        let source_type = source.source_type();

        let mut sources = self.sources.write().await;
        if sources.contains_key(&id) {
            return Err(Error::AlreadyExists { name: id });
        }

        let source = Arc::new(RegisteredSource {
            id: id.clone(),
            source_type,
            mode,
            trust_level,
            state: Arc::new(RwLock::new(SourceLifecycleState::Registered)),
            source: Arc::new(Mutex::new(source)),
            task: Arc::new(Mutex::new(None)),
            paused_by_backpressure: AtomicBool::new(false),
        });
        sources.insert(id, source);
        Ok(())
    }

    pub async fn get(&self, id: &str) -> Option<SourceSnapshot> {
        let source = self.sources.read().await.get(id).cloned()?;
        Some(source.snapshot().await)
    }

    pub async fn list(&self) -> Vec<SourceSnapshot> {
        let sources = self
            .sources
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut snapshots = Vec::with_capacity(sources.len());
        for source in sources {
            snapshots.push(source.snapshot().await);
        }
        snapshots
    }

    pub async fn start(&self, id: &str) -> AmanResult<()> {
        let source = self.find_source(id).await?;
        {
            let mut state = source.state.write().await;
            if *state == SourceLifecycleState::Running {
                return Ok(());
            }
            if *state == SourceLifecycleState::Shutdown {
                return Err(Error::InvalidStateTransition {
                    message: format!("source `{id}` is already shutdown"),
                });
            }
            *state = SourceLifecycleState::Running;
        }

        {
            let mut guard = source.source.lock().await;
            guard
                .init(Self::context_for(&source.id, source.trust_level))
                .await?;
        }

        let mut task_slot = source.task.lock().await;
        if task_slot.is_none() {
            *task_slot = Some(tokio::spawn(poll_loop(
                Arc::clone(&self.bus),
                Arc::clone(&source),
            )));
        }

        Ok(())
    }

    pub async fn pause(&self, id: &str) -> AmanResult<()> {
        let source = self.find_source(id).await?;
        {
            let mut guard = source.source.lock().await;
            guard.pause().await?;
        }
        *source.state.write().await = SourceLifecycleState::Paused;
        Ok(())
    }

    pub async fn resume(&self, id: &str) -> AmanResult<()> {
        let source = self.find_source(id).await?;
        {
            let mut state = source.state.write().await;
            if *state == SourceLifecycleState::Shutdown {
                return Err(Error::InvalidStateTransition {
                    message: format!("source `{id}` is already shutdown"),
                });
            }
            let mut guard = source.source.lock().await;
            guard.resume().await?;
            *state = SourceLifecycleState::Running;
        }
        source
            .paused_by_backpressure
            .store(false, Ordering::Release);
        Ok(())
    }

    pub async fn shutdown(&self, id: &str) -> AmanResult<()> {
        let source = self.find_source(id).await?;

        {
            let mut state = source.state.write().await;
            if *state == SourceLifecycleState::Shutdown {
                return Ok(());
            }
            *state = SourceLifecycleState::Shutdown;
        }

        if let Some(task) = source.task.lock().await.take() {
            task.abort();
        }

        let mut guard = source.source.lock().await;
        guard.shutdown().await?;
        Ok(())
    }

    pub async fn reconfigure(&self, id: &str, config: Value) -> AmanResult<()> {
        let source = self.find_source(id).await?;
        let mut guard = source.source.lock().await;
        guard.reconfigure(config).await
    }

    pub async fn unregister(&self, id: &str) -> AmanResult<()> {
        if self.get(id).await.is_none() {
            return Err(Error::NotFound {
                name: id.to_owned(),
            });
        }
        self.shutdown(id).await?;
        self.sources.write().await.remove(id);
        Ok(())
    }

    pub async fn apply_backpressure(&self, level: BackpressureLevel) -> AmanResult<()> {
        let sources = self
            .sources
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for source in sources {
            {
                let mut guard = source.source.lock().await;
                guard
                    .on_backpressure(level, &Self::context_for(&source.id, source.trust_level))
                    .await?;
            }

            if source.mode != SourceMode::Push {
                continue;
            }

            let should_pause = should_pause_push_sources(level);
            if should_pause {
                let current = *source.state.read().await;
                if current == SourceLifecycleState::Running {
                    {
                        let mut guard = source.source.lock().await;
                        guard.pause().await?;
                    }
                    *source.state.write().await = SourceLifecycleState::Paused;
                    source
                        .paused_by_backpressure
                        .store(true, Ordering::Release);
                }
                continue;
            }

            if source.paused_by_backpressure.load(Ordering::Acquire) {
                {
                    let mut guard = source.source.lock().await;
                    guard.resume().await?;
                }
                *source.state.write().await = SourceLifecycleState::Running;
                source
                    .paused_by_backpressure
                    .store(false, Ordering::Release);
            }
        }
        Ok(())
    }

    async fn find_source(&self, id: &str) -> AmanResult<Arc<RegisteredSource>> {
        self.sources
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound {
                name: id.to_owned(),
            })
    }

    fn context_for(source_id: &str, trust_level: TrustLevel) -> SourceContext {
        let mut context = SourceContext {
            base: BaseContext::new(TraceId::new()),
            source_name: Some(source_id.to_owned()),
        };
        context.base.extensions.insert(
            "trust_level".to_owned(),
            Value::String(trust_level.as_str().to_owned()),
        );
        context
    }
}

async fn poll_loop(bus: Arc<dyn EventBus>, source: Arc<RegisteredSource>) {
    loop {
        let state = *source.state.read().await;
        if state == SourceLifecycleState::Shutdown {
            break;
        }
        if state == SourceLifecycleState::Paused || !bus.can_poll() {
            sleep(Duration::from_millis(20)).await;
            continue;
        }

        let ctx = SourceRegistry::context_for(&source.id, source.trust_level);
        let events = {
            let mut guard = source.source.lock().await;
            match guard.poll(&ctx).await {
                Ok(events) => events,
                Err(_) => {
                    sleep(Duration::from_millis(50)).await;
                    continue;
                }
            }
        };

        if events.is_empty() {
            sleep(Duration::from_millis(10)).await;
            continue;
        }

        for event in events {
            let event = attach_trust_level(event, source.trust_level);
            if bus.publish(event).await.is_err() {
                sleep(Duration::from_millis(20)).await;
            }
        }

        // Minimum delay to prevent tight-loop flooding from sources that
        // always return events from poll() (e.g. the IdleDetector).
        sleep(Duration::from_millis(10)).await;
    }
}

fn attach_trust_level(mut event: Event, trust_level: TrustLevel) -> Event {
    // Set the native trust_level field (primary mechanism — used by the
    // event bus for enforcement).
    event.trust_level = Some(trust_level);

    // Also inject into payload for backward compatibility with consumers
    // that read _aman_trust_level from the payload.
    let value = Value::String(trust_level.as_str().to_owned());
    if let Some(object) = event.payload.as_object_mut() {
        object.insert("_aman_trust_level".to_owned(), value);
        return event;
    }
    event.payload = json!({
        "_aman_trust_level": trust_level.as_str(),
        "data": event.payload,
    });
    event
}

#[must_use]
pub const fn should_pause_push_sources(level: BackpressureLevel) -> bool {
    matches!(
        level,
        BackpressureLevel::L3
            | BackpressureLevel::L4A
            | BackpressureLevel::L4B
            | BackpressureLevel::Critical
    )
}
