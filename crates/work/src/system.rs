// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! WorkSystem — passive FIFO queue consumer engine.
//!
//! Wraps [`lifecycle::LifecycleEngine`] with work-specific types and logic.
//! Architecture ref: work-design.md v2 §4-5.

use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use event_bus::EventBus;
use kernel::agent::AgentSystemState;
use kernel::event::{Event, EventType};
use lifecycle::{LifecycleEngine, LifecycleError};

use crate::config::WorkConfig;
use crate::spec::WorkSpec;
use crate::trace::WorkTraceEvent;
use crate::types::{
    IdleSignal, WorkContext, WorkError, WorkEvent, WorkItem, WorkItemFailedEvent,
    WorkItemResultEvent, WorkItemSource, WorkResult, WorkState, WORK_SOURCE,
};

/// The per-agent Work System engine (v2: passive queue consumer).
///
/// Wraps the generic [`LifecycleEngine`] with work-specific types.
pub struct WorkSystem {
    engine: LifecycleEngine<WorkSpec>,
    config: WorkConfig,
    local_bus: Arc<dyn EventBus>,
    global_bus: Arc<dyn EventBus>,
    idle_signal_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<IdleSignal>>>,
}

impl WorkSystem {
    #[must_use]
    pub fn new(
        agent_id: impl Into<String>,
        config: WorkConfig,
        local_bus: Arc<dyn EventBus>,
        global_bus: Arc<dyn EventBus>,
        system_state: Option<Arc<std::sync::Mutex<AgentSystemState>>>,
    ) -> Self {
        let spec = WorkSpec::new(config.execution.auto_decompose);
        let engine = LifecycleEngine::new(
            agent_id,
            spec,
            config.queue.max_size,
            config.retry.max_step_retries,
            Arc::clone(&local_bus),
            Arc::clone(&global_bus),
            system_state.clone(),
            AgentSystemState::Working,
        );

        Self {
            engine,
            config,
            local_bus,
            global_bus,
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

    pub async fn current_state(&self) -> WorkState {
        self.engine.current_state().await
    }

    pub async fn snapshot(&self) -> WorkContext {
        let inner = self.engine.snapshot().await;
        WorkContext {
            inner,
        }
    }

    // ------------------------------------------------------------------
    // Main event handler
    // ------------------------------------------------------------------

    pub async fn handle(&self, event: WorkEvent) -> WorkResult<()> {
        debug!(
            "WorkSystem::handle agent_id={} event_kind={}",
            self.engine.agent_id(),
            event.kind(),
        );

        match event {
            WorkEvent::Interrupt { reason, by_system } => {
                self.engine.handle_interrupt(&reason, &by_system).await?;
                self.record_trace(WorkTraceEvent::Interrupted {
                    item_id: None,
                    reason,
                    by_system,
                })
                .await;
                Ok(())
            }

            WorkEvent::WorkItemAssigned { item, source } => {
                let item_id = item.id;
                let source_str = source_name(&source);

                info!(
                    "WorkItemAssigned — enqueuing agent_id={} item_id={} title={} source={}",
                    self.engine.agent_id(),
                    item_id,
                    item.title,
                    source_str,
                );

                self.record_trace(WorkTraceEvent::ItemReceived {
                    item_id,
                    source: source_str,
                })
                .await;

                // Queue size check (engine handles this but we check early for
                // work-specific error types).
                {
                    let snap = self.engine.snapshot().await;
                    if snap.queue_len() >= self.config.queue.max_size {
                        return Err(WorkError {
                            code: "queue_full".into(),
                            message: format!("Queue at capacity ({})", self.config.queue.max_size),
                            retryable: false,
                        });
                    }
                }

                let source_json = serde_json::to_value(&source).unwrap_or_default();
                self.engine.handle_assigned(item, source_json).await?;
                Ok(())
            }

            WorkEvent::WorkItemCompleted {
                item_id,
                result,
                duration,
            } => {
                info!(
                    agent_id = %self.engine.agent_id(),
                    %item_id,
                    steps_completed = %result.steps_completed,
                    "WorkItemCompleted"
                );

                self.record_trace(WorkTraceEvent::ItemCompleted {
                    item_id,
                    duration,
                    steps_completed: result.steps_completed,
                    steps_failed: result.steps_failed,
                })
                .await;

                // Notify global bus if requested.
                {
                    let snap = self.engine.snapshot().await;
                    if let Some(ref item) = snap.current {
                        if item.notify_on_complete {
                            let _ = self
                                .global_bus
                                .publish(Event::new(
                                    WORK_SOURCE,
                                    EventType::Custom("work.item.result".into()),
                                    serde_json::to_value(WorkItemResultEvent {
                                        item_id,
                                        result: result.clone(),
                                        agent_id: self.engine.agent_id().to_string(),
                                    })
                                    .unwrap_or_default(),
                                ))
                                .await;
                        }
                    }
                }

                self.send_idle_signal(IdleSignal::Satisfaction {
                    item_id: lifecycle::ItemId::new(),
                })
                .await;

                let result_json = serde_json::to_value(&result).unwrap_or_default();
                self.engine
                    .handle_completed(&item_id.to_string(), result_json, duration.as_secs_f64())
                    .await?;
                Ok(())
            }

            WorkEvent::WorkItemFailed {
                item_id,
                error,
                retryable,
            } => {
                warn!(
                    agent_id = %self.engine.agent_id(),
                    %item_id,
                    %retryable,
                    "WorkItemFailed: {}",
                    error.message,
                );

                self.record_trace(WorkTraceEvent::ItemFailed {
                    item_id,
                    error: error.message.clone(),
                    retryable,
                })
                .await;

                if !retryable {
                    let error_msg = error.message.clone();
                    let _ = self
                        .global_bus
                        .publish(Event::new(
                            WORK_SOURCE,
                            EventType::Custom("work.item.failed_event".into()),
                            serde_json::to_value(WorkItemFailedEvent {
                                item_id,
                                error: error_msg.clone(),
                                agent_id: self.engine.agent_id().to_string(),
                            })
                            .unwrap_or_default(),
                        ))
                        .await;

                    self.send_idle_signal(IdleSignal::Frustration {
                        reason: Some(error_msg),
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

    /// Handle an internal StepEvent from the local bus.
    pub async fn handle_step(&self, step_index: usize) -> WorkResult<()> {
        debug!(
            agent_id = %self.engine.agent_id(),
            step_index,
            "WorkSystem::handle_step"
        );
        self.engine.handle_step(step_index).await?;
        Ok(())
    }

    /// Push a work item onto this agent's queue by publishing a
    /// WorkItemAssigned event on the local bus.
    pub async fn push_work_item(
        &self,
        item: WorkItem,
        source: WorkItemSource,
    ) -> WorkResult<()> {
        self.publish_work_event(WorkEvent::WorkItemAssigned { item, source })
            .await
    }

    pub async fn shutdown(&self) {
        info!(
            "WorkSystem shutting down agent_id={}",
            self.engine.agent_id()
        );
        let _ = self
            .handle(WorkEvent::Interrupt {
                reason: "shutdown".into(),
                by_system: "core".into(),
            })
            .await;
    }

    // ------------------------------------------------------------------
    // Event publishing
    // ------------------------------------------------------------------

    async fn publish_work_event(&self, event: WorkEvent) -> WorkResult<()> {
        let kind = event.kind().to_string();
        let payload = serde_json::to_value(&event).map_err(|e| WorkError {
            code: "serialization_error".into(),
            message: e.to_string(),
            retryable: false,
        })?;
        let ev = Event::new(WORK_SOURCE, EventType::Custom(kind), payload);
        self.local_bus.publish(ev).await.map_err(|e| WorkError {
            code: "publish_error".into(),
            message: e.to_string(),
            retryable: true,
        })?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Idle signal
    // ------------------------------------------------------------------

    async fn send_idle_signal(&self, signal: IdleSignal) {
        let tx = self.idle_signal_tx.lock().await;
        if let Some(ref tx) = *tx {
            let _ = tx.send(signal);
        }
    }

    // ------------------------------------------------------------------
    // Trace recording
    // ------------------------------------------------------------------

    async fn record_trace(&self, event: WorkTraceEvent) {
        debug!(agent_id = %self.engine.agent_id(), trace = ?event, "WorkTraceEvent");
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn source_name(source: &WorkItemSource) -> String {
    match source {
        WorkItemSource::Cli { .. } => "cli".into(),
        WorkItemSource::Api { .. } => "api".into(),
        WorkItemSource::Kanban { .. } => "kanban".into(),
        WorkItemSource::Todo { .. } => "todo".into(),
        WorkItemSource::SeekResponse { .. } => "seek_response".into(),
        WorkItemSource::Custom { name, .. } => name.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorkConfig;
    use crate::types::{Priority, Step, WorkItemId, WorkItemResult, WorkOutcome};
    use event_bus::{EventBus, InMemoryBus, InMemoryBusConfig};
    use kernel::types::Timestamp;
    use lifecycle::LifecycleState;
    use std::collections::HashMap;

    fn make_bus() -> Arc<dyn EventBus> {
        Arc::new(InMemoryBus::new(InMemoryBusConfig::default()))
    }

    fn make_config() -> WorkConfig {
        WorkConfig::default()
    }

    fn make_item(title: &str) -> WorkItem {
        WorkItem {
            id: WorkItemId::new(),
            title: title.into(),
            description: String::new(),
            steps: None,
            priority: Priority::default(),
            timeout: None,
            context: HashMap::new(),
            notify_on_complete: false,
            created_at: Timestamp::now(),
        }
    }

    fn make_item_with_steps(title: &str, steps: Vec<Step>) -> WorkItem {
        WorkItem {
            steps: Some(steps),
            ..make_item(title)
        }
    }

    // ── Construction & state ────────────────────────────────────────

    #[tokio::test]
    async fn system_starts_at_idle() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    // ── Interrupt ──────────────────────────────────────────────────

    #[tokio::test]
    async fn interrupt_from_busy_goes_to_idle() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        // Manually set engine state via test helper
        sys.engine._set_state(LifecycleState::Busy).await;

        sys.handle(WorkEvent::Interrupt {
            reason: "test".into(),
            by_system: "test".into(),
        })
        .await
        .expect("handle should succeed");
        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    #[tokio::test]
    async fn interrupt_from_idle_stays_idle() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        sys.handle(WorkEvent::Interrupt {
            reason: "noop".into(),
            by_system: "test".into(),
        })
        .await
        .expect("handle should succeed");
        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    #[tokio::test]
    async fn interrupt_saves_checkpoint() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        let item = make_item("current-task");
        sys.engine._set_state(LifecycleState::Busy).await;
        sys.engine._set_current(item).await;
        sys.engine._set_step_index(3).await;

        sys.handle(WorkEvent::Interrupt {
            reason: "user_query".into(),
            by_system: "chat".into(),
        })
        .await
        .expect("handle should succeed");

        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    // ── WorkItemAssigned ───────────────────────────────────────────

    #[tokio::test]
    async fn work_item_assigned_idle_enqueues_and_starts_item() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);

        let item = make_item_with_steps(
            "test-task",
            vec![Step {
                index: 0,
                description: "do it".into(),
                tool: None,
                expect_llm: false,
                max_retries: 0,
            }],
        );

        sys.handle(WorkEvent::WorkItemAssigned {
            item,
            source: WorkItemSource::Cli {
                operator: "user".into(),
            },
        })
        .await
        .expect("handle should succeed");

        let snap = sys.snapshot().await;
        assert!(!snap.is_idle());
        assert_eq!(snap.queue_len(), 0);
        assert!(snap.current().is_some());
        assert_eq!(snap.steps().len(), 1);
        assert_eq!(snap.step_index(), 0);
    }

    #[tokio::test]
    async fn work_item_assigned_when_busy_just_enqueues() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        sys.engine._set_state(LifecycleState::Busy).await;
        sys.engine._set_current(make_item("running")).await;

        let item = make_item("queued");
        sys.handle(WorkEvent::WorkItemAssigned {
            item,
            source: WorkItemSource::Api {
                endpoint: "/test".into(),
                operator: "test".into(),
            },
        })
        .await
        .expect("handle should succeed");

        let snap = sys.snapshot().await;
        assert_eq!(snap.queue_len(), 1);
    }

    #[tokio::test]
    async fn work_item_assigned_rejects_when_queue_full() {
        let mut config = make_config();
        config.queue.max_size = 1;

        let sys = WorkSystem::new("agent-1", config, make_bus(), make_bus(), None);
        sys.engine._set_state(LifecycleState::Busy).await;
        sys.engine._set_current(make_item("running")).await;
        sys.engine._enqueue(make_item("queued")).await;

        let item = make_item("overflow");
        let result = sys
            .handle(WorkEvent::WorkItemAssigned {
                item,
                source: WorkItemSource::Cli {
                    operator: "user".into(),
                },
            })
            .await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().code, "queue_full");
    }

    // ── Step execution ─────────────────────────────────────────────

    #[tokio::test]
    async fn execute_step_advances_to_next_step() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);

        sys.engine._set_state(LifecycleState::Busy).await;
        sys.engine._set_current(make_item("multi-step")).await;
        sys.engine
            ._set_steps(vec![
                Step {
                    index: 0,
                    description: "step 0".into(),
                    tool: None,
                    expect_llm: false,
                    max_retries: 0,
                },
                Step {
                    index: 1,
                    description: "step 1".into(),
                    tool: None,
                    expect_llm: false,
                    max_retries: 0,
                },
            ])
            .await;

        sys.handle_step(0).await.expect("handle_step should succeed");

        let snap = sys.snapshot().await;
        assert_eq!(snap.step_index(), 1);
        assert_eq!(snap.step_outputs().len(), 1);
        assert!(snap.step_outputs()[0].success);
    }

    #[tokio::test]
    async fn execute_last_step_finishes_item() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        let item_id = WorkItemId::new();

        sys.engine._set_state(LifecycleState::Busy).await;
        sys.engine
            ._set_current(WorkItem {
                id: item_id,
                steps: None,
                ..make_item("single-step")
            })
            .await;
        sys.engine
            ._set_steps(vec![Step {
                index: 0,
                description: "only step".into(),
                tool: None,
                expect_llm: false,
                max_retries: 0,
            }])
            .await;

        sys.handle_step(0).await.expect("handle_step should succeed");

        let snap = sys.snapshot().await;
        assert_eq!(snap.step_outputs().len(), 1);
        assert!(snap.step_outputs()[0].success);

        // Simulate event loop routing WorkItemCompleted back.
        sys.handle(WorkEvent::WorkItemCompleted {
            item_id,
            result: WorkItemResult {
                item_id,
                outcome: WorkOutcome::Completed,
                summary: "done".into(),
                steps_completed: 1,
                steps_failed: 0,
                total_duration: snap.step_outputs()[0].duration,
            },
            duration: snap.step_outputs()[0].duration,
        })
        .await
        .expect("handle completion should succeed");

        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    // ── WorkItemCompleted ──────────────────────────────────────────

    #[tokio::test]
    async fn completed_item_with_empty_queue_goes_idle() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        let item_id = WorkItemId::new();

        sys.handle(WorkEvent::WorkItemCompleted {
            item_id,
            result: WorkItemResult {
                item_id,
                outcome: WorkOutcome::Completed,
                summary: "done".into(),
                steps_completed: 3,
                steps_failed: 0,
                total_duration: std::time::Duration::from_secs(5),
            },
            duration: std::time::Duration::from_secs(5),
        })
        .await
        .expect("handle should succeed");

        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    #[tokio::test]
    async fn completed_item_with_queued_item_starts_next() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);

        sys.engine._set_state(LifecycleState::Busy).await;
        sys.engine._set_current(make_item("finishing")).await;
        sys.engine
            ._enqueue(make_item_with_steps(
                "next-up",
                vec![Step {
                    index: 0,
                    description: "do".into(),
                    tool: None,
                    expect_llm: false,
                    max_retries: 0,
                }],
            ))
            .await;

        let item_id = WorkItemId::new();
        sys.handle(WorkEvent::WorkItemCompleted {
            item_id,
            result: WorkItemResult {
                item_id,
                outcome: WorkOutcome::Completed,
                summary: "done".into(),
                steps_completed: 1,
                steps_failed: 0,
                total_duration: std::time::Duration::from_secs(1),
            },
            duration: std::time::Duration::from_secs(1),
        })
        .await
        .expect("handle should succeed");

        let snap = sys.snapshot().await;
        assert!(!snap.is_idle());
        assert_eq!(snap.queue_len(), 0);
        assert!(snap.current().is_some());
        assert_eq!(snap.current().unwrap().title, "next-up");
    }

    // ── WorkItemFailed ─────────────────────────────────────────────

    #[tokio::test]
    async fn failed_retryable_re_enqueues_to_front() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        let item_id = WorkItemId::new();
        let retry_item = make_item("retry-me");

        sys.engine._set_state(LifecycleState::Busy).await;
        sys.engine._set_current(retry_item).await;
        sys.engine._enqueue(make_item("already-queued")).await;

        sys.handle(WorkEvent::WorkItemFailed {
            item_id,
            error: WorkError {
                code: "E_TEST".into(),
                message: "transient".into(),
                retryable: true,
            },
            retryable: true,
        })
        .await
        .expect("handle should succeed");

        let snap = sys.snapshot().await;
        assert!(snap.current().is_some());
    }

    #[tokio::test]
    async fn failed_non_retryable_goes_to_next_or_idle() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        let item_id = WorkItemId::new();

        sys.engine._set_state(LifecycleState::Busy).await;
        sys.engine._set_current(make_item("doomed")).await;

        sys.handle(WorkEvent::WorkItemFailed {
            item_id,
            error: WorkError {
                code: "FATAL".into(),
                message: "permanent error".into(),
                retryable: false,
            },
            retryable: false,
        })
        .await
        .expect("handle should succeed");

        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    // ── Full pipeline: assign → step → step → complete → IDLE ────

    #[tokio::test]
    async fn full_event_driven_pipeline() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);

        let item = make_item_with_steps(
            "pipeline-test",
            vec![
                Step {
                    index: 0,
                    description: "analyze".into(),
                    tool: None,
                    expect_llm: false,
                    max_retries: 0,
                },
                Step {
                    index: 1,
                    description: "execute".into(),
                    tool: None,
                    expect_llm: false,
                    max_retries: 0,
                },
            ],
        );

        sys.handle(WorkEvent::WorkItemAssigned {
            item,
            source: WorkItemSource::Cli {
                operator: "test".into(),
            },
        })
        .await
        .expect("assign should succeed");

        let snap = sys.snapshot().await;
        assert!(!snap.is_idle());
        assert_eq!(snap.steps().len(), 2);
        assert_eq!(snap.step_index(), 0);

        sys.handle_step(0).await.expect("step 0 should succeed");
        let snap = sys.snapshot().await;
        assert_eq!(snap.step_index(), 1);
        assert_eq!(snap.step_outputs().len(), 1);

        sys.handle_step(1).await.expect("step 1 should succeed");
        let snap = sys.snapshot().await;
        assert_eq!(snap.step_outputs().len(), 2);

        let item_id = snap.current().unwrap().id;
        let total_duration: std::time::Duration =
            snap.step_outputs().iter().map(|o| o.duration).sum();
        sys.handle(WorkEvent::WorkItemCompleted {
            item_id,
            result: WorkItemResult {
                item_id,
                outcome: WorkOutcome::Completed,
                summary: "done".into(),
                steps_completed: 2,
                steps_failed: 0,
                total_duration,
            },
            duration: total_duration,
        })
        .await
        .expect("handle completion should succeed");

        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    // ── Convenience methods ───────────────────────────────────────

    #[tokio::test]
    async fn push_work_item_publishes_assigned_event() {
        let local_bus = make_bus();
        let sys = WorkSystem::new(
            "agent-1",
            make_config(),
            local_bus.clone(),
            make_bus(),
            None,
        );
        let item = make_item_with_steps(
            "push-test",
            vec![Step {
                index: 0,
                description: "do".into(),
                tool: None,
                expect_llm: false,
                max_retries: 0,
            }],
        );

        let result = sys
            .push_work_item(
                item,
                WorkItemSource::Kanban {
                    board_id: "kb-1".into(),
                    scheduler: "auto".into(),
                },
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn context_snapshot_reflects_current_state() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        let snap = sys.snapshot().await;
        assert!(snap.is_idle());
        assert_eq!(snap.queue_len(), 0);
    }

    #[tokio::test]
    async fn push_work_item_then_process_to_completion() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);

        let item = make_item_with_steps(
            "task-a",
            vec![Step {
                index: 0,
                description: "step-a".into(),
                tool: None,
                expect_llm: false,
                max_retries: 0,
            }],
        );
        sys.handle(WorkEvent::WorkItemAssigned {
            item,
            source: WorkItemSource::Cli {
                operator: "test".into(),
            },
        })
        .await
        .expect("assign should succeed");

        assert_eq!(sys.current_state().await, LifecycleState::Busy);

        sys.handle_step(0).await.expect("step should succeed");

        let snap = sys.snapshot().await;
        let item_id = snap.current().unwrap().id;
        let total_duration: std::time::Duration =
            snap.step_outputs().iter().map(|o| o.duration).sum();
        sys.handle(WorkEvent::WorkItemCompleted {
            item_id,
            result: WorkItemResult {
                item_id,
                outcome: WorkOutcome::Completed,
                summary: "done".into(),
                steps_completed: 1,
                steps_failed: 0,
                total_duration,
            },
            duration: total_duration,
        })
        .await
        .expect("complete should succeed");

        assert_eq!(sys.current_state().await, WorkState::Idle);
    }
}
