// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! StudySystem — passive FIFO learning queue consumer.
//!
//! Wraps [`lifecycle::LifecycleEngine`] with study-specific types and logic.
//! Architecture ref: study-design.md v2 §4-5.

use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use event_bus::EventBus;
use kernel::agent::AgentSystemState;
use kernel::event::{Event, EventType};
use lifecycle::{LifecycleEngine, LifecycleError};

use crate::config::StudyConfig;
use crate::spec::StudySpec;
use crate::trace::StudyTraceEvent;
use crate::types::{
    IdleSignal, Priority, StudyContext, StudyDepth, StudyError, StudyEvent, StudyItem,
    StudyItemId, StudyItemSource, StudyOutcome, StudyResult, StudyState, STUDY_SOURCE,
};
use kernel::types::Timestamp;
use std::collections::HashMap;

/// The per-agent Study System engine.
pub struct StudySystem {
    engine: LifecycleEngine<StudySpec>,
    config: StudyConfig,
    local_bus: Arc<dyn EventBus>,
    _global_bus: Arc<dyn EventBus>,
    idle_signal_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<IdleSignal>>>,
}

impl StudySystem {
    #[must_use]
    pub fn new(
        agent_id: impl Into<String>,
        config: StudyConfig,
        local_bus: Arc<dyn EventBus>,
        global_bus: Arc<dyn EventBus>,
        system_state: Option<Arc<std::sync::Mutex<AgentSystemState>>>,
    ) -> Self {
        let spec = StudySpec::new(config.execution.auto_decompose);
        let engine = LifecycleEngine::new(
            agent_id,
            spec,
            config.queue.max_size,
            config.retry.max_step_retries,
            Arc::clone(&local_bus),
            Arc::clone(&global_bus),
            system_state,
            AgentSystemState::Studying,
        );

        Self {
            engine,
            config,
            local_bus,
            _global_bus: global_bus,
            idle_signal_tx: Mutex::new(None),
        }
    }

    pub async fn set_idle_signal_tx(
        &self,
        tx: tokio::sync::mpsc::UnboundedSender<IdleSignal>,
    ) {
        *self.idle_signal_tx.lock().await = Some(tx.clone());
        self.engine.set_idle_signal_tx(tx).await;
    }

    #[must_use]
    pub fn agent_id(&self) -> &str {
        self.engine.agent_id()
    }

    pub async fn current_state(&self) -> StudyState {
        self.engine.current_state().await
    }

    pub async fn snapshot(&self) -> StudyContext {
        let inner = self.engine.snapshot().await;
        StudyContext {
            inner,
            learning_path: None,
            accumulated_notes: Default::default(),
        }
    }

    // ------------------------------------------------------------------
    // Main event handler
    // ------------------------------------------------------------------

    pub async fn handle(&self, event: StudyEvent) -> StudyResult<()> {
        debug!(
            "StudySystem::handle agent_id={} event_kind={}",
            self.engine.agent_id(),
            event.kind(),
        );

        match event {
            StudyEvent::Interrupt { reason, by_system } => {
                self.engine.handle_interrupt(&reason, &by_system).await?;
                self.record_trace(StudyTraceEvent::Interrupted {
                    item_id: None,
                    reason,
                    by_system,
                })
                .await;
                Ok(())
            }

            StudyEvent::StudyItemAssigned { item, source } => {
                let item_id = item.id;
                let source_str = source_name(&source);

                info!(
                    "StudyItemAssigned — enqueuing agent_id={} item_id={} topic={} source={}",
                    self.engine.agent_id(),
                    item_id,
                    item.topic,
                    source_str,
                );

                self.record_trace(StudyTraceEvent::ItemReceived {
                    item_id,
                    topic: item.topic.clone(),
                    source: source_str,
                })
                .await;

                {
                    let snap = self.engine.snapshot().await;
                    if snap.queue_len() >= self.config.queue.max_size {
                        return Err(StudyError {
                            code: "queue_full".into(),
                            message: format!(
                                "Study queue at capacity ({})",
                                self.config.queue.max_size
                            ),
                            retryable: false,
                        });
                    }
                }

                let source_json = serde_json::to_value(&source).unwrap_or_default();
                self.engine.handle_assigned(item, source_json).await?;
                Ok(())
            }

            StudyEvent::StudyItemCompleted {
                item_id,
                outcome,
                duration,
            } => {
                info!(
                    agent_id = %self.engine.agent_id(),
                    %item_id,
                    ?outcome,
                    "StudyItemCompleted"
                );

                let comprehension = match &outcome {
                    StudyOutcome::Completed { comprehension } => *comprehension,
                    _ => 0.0,
                };

                self.record_trace(StudyTraceEvent::ItemCompleted {
                    item_id,
                    duration,
                    comprehension,
                })
                .await;

                self.send_idle_signal(IdleSignal::Satisfaction {
                    item_id: lifecycle::ItemId::new(),
                })
                .await;

                let outcome_json = serde_json::to_value(&outcome).unwrap_or_default();
                self.engine
                    .handle_completed(
                        &item_id.to_string(),
                        outcome_json,
                        duration.as_secs_f64(),
                    )
                    .await?;
                Ok(())
            }

            StudyEvent::StudyItemFailed {
                item_id,
                error,
                retryable,
            } => {
                warn!(
                    agent_id = %self.engine.agent_id(),
                    %item_id,
                    %retryable,
                    "StudyItemFailed: {}",
                    error.message,
                );

                self.record_trace(StudyTraceEvent::ItemFailed {
                    item_id,
                    error: error.message.clone(),
                    retryable,
                })
                .await;

                if !retryable {
                    self.send_idle_signal(IdleSignal::Frustration {
                        reason: Some(error.message.clone()),
                    })
                    .await;
                }

                let lc_error = LifecycleError {
                    code: error.code,
                    message: error.message,
                    retryable: error.retryable,
                };
                self.engine
                    .handle_failed(&item_id.to_string(), lc_error, retryable)
                    .await?;
                Ok(())
            }
        }
    }

    /// Handle an internal phase step event.
    pub async fn handle_phase(&self, phase_index: usize) -> StudyResult<()> {
        debug!(
            agent_id = %self.engine.agent_id(),
            phase_index,
            "StudySystem::handle_phase"
        );
        self.engine.handle_step(phase_index).await?;
        Ok(())
    }

    /// Handle a boredom action tag. Pushes a curiosity-driven study item.
    pub async fn on_boredom_action(&self, _tag: &str) {
        let topic = "探索感兴趣的主题";
        info!("random_hit:action: {topic}");

        let item = StudyItem {
            id: StudyItemId::new(),
            topic: topic.into(),
            materials: None,
            depth: StudyDepth::Read,
            priority: Priority::Low,
            timeout: None,
            context: {
                let mut ctx = HashMap::new();
                ctx.insert(
                    "source".into(),
                    serde_json::to_value(StudyItemSource::IdleExploration {
                        curiosity_topic: "boredom".into(),
                    })
                    .unwrap_or_default(),
                );
                ctx
            },
            notify_on_complete: false,
            created_at: Timestamp::now(),
        };
        let _ = self
            .push_study_item(
                item,
                StudyItemSource::IdleExploration {
                    curiosity_topic: "boredom".into(),
                },
            )
            .await;
    }

    /// Push a study item onto this agent's queue.
    pub async fn push_study_item(
        &self,
        item: StudyItem,
        source: StudyItemSource,
    ) -> StudyResult<()> {
        let event = StudyEvent::StudyItemAssigned { item, source };
        let kind = event.kind().to_string();
        let payload =
            serde_json::to_value(&event).map_err(|e| StudyError {
                code: "serialization_error".into(),
                message: e.to_string(),
                retryable: false,
            })?;
        let ev = Event::new(STUDY_SOURCE, EventType::Custom(kind), payload);
        self.local_bus.publish(ev).await.map_err(|e| StudyError {
            code: "publish_error".into(),
            message: e.to_string(),
            retryable: true,
        })?;
        Ok(())
    }

    pub async fn shutdown(&self) {
        info!(
            "StudySystem shutting down agent_id={}",
            self.engine.agent_id()
        );
        let _ = self.engine.handle_interrupt("shutdown", "core").await;
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    async fn send_idle_signal(&self, signal: IdleSignal) {
        let tx = self.idle_signal_tx.lock().await;
        if let Some(ref tx) = *tx {
            let _ = tx.send(signal);
        }
    }

    async fn record_trace(&self, event: StudyTraceEvent) {
        debug!(agent_id = %self.engine.agent_id(), trace = ?event, "StudyTraceEvent");
    }

    // ------------------------------------------------------------------
    // Test helpers
    // ------------------------------------------------------------------

    #[doc(hidden)]
    pub async fn _set_state(&self, state: StudyState) {
        self.engine._set_state(state).await;
    }

    #[doc(hidden)]
    pub async fn _set_current(&self, item: StudyItem) {
        self.engine._set_current(item).await;
    }

    #[doc(hidden)]
    pub async fn _set_phases(&self, phases: Vec<crate::types::StudyPhase>) {
        self.engine._set_steps(phases).await;
    }

    #[doc(hidden)]
    pub async fn _set_phase_index(&self, idx: usize) {
        self.engine._set_step_index(idx).await;
    }

    #[doc(hidden)]
    pub async fn _enqueue(&self, item: StudyItem) {
        self.engine._enqueue(item).await;
    }
}

fn source_name(source: &StudyItemSource) -> String {
    match source {
        StudyItemSource::UserAssigned { .. } => "user_assigned".into(),
        StudyItemSource::IdleExploration { .. } => "idle_exploration".into(),
        StudyItemSource::MaterialSubscription { .. } => "material_subscription".into(),
        StudyItemSource::ScheduledReview { .. } => "scheduled_review".into(),
        StudyItemSource::SeekResponse { .. } => "seek_response".into(),
        StudyItemSource::Custom { name, .. } => name.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::StudyConfig;
    use crate::types::{Priority, StudyDepth, StudyItemId, StudyPhase};
    use event_bus::{EventBus, InMemoryBus, InMemoryBusConfig};
    use kernel::types::Timestamp;
    use lifecycle::LifecycleState;
    use std::collections::HashMap;

    fn make_bus() -> Arc<dyn EventBus> {
        Arc::new(InMemoryBus::new(InMemoryBusConfig::default()))
    }

    fn make_config() -> StudyConfig {
        StudyConfig::default()
    }

    fn make_item(topic: &str, depth: StudyDepth) -> StudyItem {
        StudyItem {
            id: StudyItemId::new(),
            topic: topic.into(),
            materials: Some(vec![]),
            depth,
            priority: Priority::default(),
            timeout: None,
            context: HashMap::new(),
            notify_on_complete: false,
            created_at: Timestamp::now(),
        }
    }

    #[tokio::test]
    async fn system_starts_at_idle() {
        let sys = StudySystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        assert_eq!(sys.current_state().await, StudyState::Idle);
    }

    #[tokio::test]
    async fn interrupt_goes_to_idle() {
        let sys = StudySystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        sys._set_state(LifecycleState::Busy).await;

        sys.handle(StudyEvent::Interrupt {
            reason: "test".into(),
            by_system: "test".into(),
        })
        .await
        .expect("handle should succeed");
        assert_eq!(sys.current_state().await, StudyState::Idle);
    }

    #[tokio::test]
    async fn assigned_idle_enqueues_and_starts() {
        let sys = StudySystem::new("agent-1", make_config(), make_bus(), make_bus(), None);

        let item = make_item("Rust async", StudyDepth::Read);
        sys.handle(StudyEvent::StudyItemAssigned {
            item,
            source: StudyItemSource::UserAssigned {
                operator: "user".into(),
            },
        })
        .await
        .expect("assign should succeed");

        let snap = sys.snapshot().await;
        assert!(!snap.is_idle());
        assert!(snap.current().is_some());
        assert!(!snap.phases().is_empty());
    }

    #[tokio::test]
    async fn assigned_when_busy_just_enqueues() {
        let sys = StudySystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        sys._set_state(LifecycleState::Busy).await;
        sys._set_current(make_item("running", StudyDepth::Read))
            .await;

        let item = make_item("queued", StudyDepth::Skim);
        sys.handle(StudyEvent::StudyItemAssigned {
            item,
            source: StudyItemSource::MaterialSubscription {
                feed_url: "arxiv".into(),
            },
        })
        .await
        .expect("assign should succeed");

        let snap = sys.snapshot().await;
        assert_eq!(snap.queue_len(), 1);
    }

    #[tokio::test]
    async fn execute_phase_advances() {
        let sys = StudySystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        sys._set_state(LifecycleState::Busy).await;
        sys._set_current(make_item("multi-phase", StudyDepth::Read))
            .await;
        sys._set_phases(vec![
            StudyPhase::GatherMaterials,
            StudyPhase::Plan,
            StudyPhase::LearnModule { index: 0 },
        ])
        .await;

        sys.handle_phase(0).await.expect("phase 0 should succeed");

        let snap = sys.snapshot().await;
        assert_eq!(snap.phase_index(), 1);
        assert_eq!(snap.step_outputs().len(), 1);
        assert!(snap.step_outputs()[0].success);
    }

    #[tokio::test]
    async fn last_phase_completes_item() {
        let sys = StudySystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        let item_id = StudyItemId::new();

        sys._set_state(LifecycleState::Busy).await;
        sys._set_current(StudyItem {
            id: item_id,
            ..make_item("single-phase", StudyDepth::Skim)
        })
        .await;
        sys._set_phases(vec![StudyPhase::GatherMaterials]).await;

        sys.handle_phase(0).await.expect("phase should succeed");

        let snap = sys.snapshot().await;
        assert_eq!(snap.step_outputs().len(), 1);

        // Simulate event loop routing completion back.
        sys.handle(StudyEvent::StudyItemCompleted {
            item_id,
            outcome: StudyOutcome::Completed {
                comprehension: 0.9,
            },
            duration: std::time::Duration::from_secs(1),
        })
        .await
        .expect("handle completion should succeed");

        assert_eq!(sys.current_state().await, StudyState::Idle);
    }

    #[tokio::test]
    async fn deep_item_generates_more_phases() {
        let sys = StudySystem::new("agent-1", make_config(), make_bus(), make_bus(), None);

        let item = make_item("Deep learning topic", StudyDepth::Deep);
        sys.handle(StudyEvent::StudyItemAssigned {
            item,
            source: StudyItemSource::UserAssigned {
                operator: "user".into(),
            },
        })
        .await
        .expect("assign should succeed");

        let snap = sys.snapshot().await;
        // Deep: Plan + 4 LearnModules + Practice + Consolidate
        assert!(snap.phases().len() > 4);
        assert!(snap
            .phases()
            .iter()
            .any(|p| matches!(p, StudyPhase::Practice)));
    }

    #[tokio::test]
    async fn push_study_item_publishes_event() {
        let local_bus = make_bus();
        let sys = StudySystem::new(
            "agent-1",
            make_config(),
            local_bus.clone(),
            make_bus(),
            None,
        );
        let item = make_item("push-test", StudyDepth::Read);

        let result = sys
            .push_study_item(
                item,
                StudyItemSource::SeekResponse {
                    request_id: "req-1".into(),
                },
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn context_snapshot_reflects_state() {
        let sys = StudySystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        let snap = sys.snapshot().await;
        assert!(snap.is_idle());
        assert_eq!(snap.queue_len(), 0);
    }
}
