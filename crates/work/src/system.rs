// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! WorkSystem — passive queue consumer engine.
//!
//! Architecture ref: work-design.md v2 §4-5

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use event_bus::EventBus;
use kernel::agent::AgentSystemState;
use kernel::event::{Event, EventType};
use crate::config::WorkConfig;
use crate::trace::WorkTraceEvent;
use crate::types::{
    IdleSignal, StepOutput, WorkContext, WorkEvent, WorkError, WorkItem,
    WorkItemId, WorkItemResult, WorkItemSource, WorkOutcome, WorkResult, WorkState,
    WORK_SOURCE,
};

// ---------------------------------------------------------------------------
// WorkSystem
// ---------------------------------------------------------------------------

/// The per-agent Work System engine.
///
/// v2: passive queue consumer. External systems push [`WorkEvent::WorkItemAssigned`]
/// onto the event bus; the Work System consumes them via [`handle`]. No polling,
/// no claim competition, no board client — a pure FIFO consumer.
pub struct WorkSystem {
    /// This agent's identifier.
    agent_id: String,

    /// Work configuration.
    #[allow(dead_code)]
    config: WorkConfig,

    /// Shared work context (state, queue, current item, steps).
    ctx: Mutex<WorkContext>,

    /// Agent's local event bus.
    local_bus: Arc<dyn EventBus>,

    /// Idle coordination: inject satisfaction/frustration signals.
    idle_signal_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<IdleSignal>>>,

    /// Shared system state for UI visibility.
    system_state: Option<Arc<std::sync::Mutex<AgentSystemState>>>,
}

impl WorkSystem {
    /// Create a new WorkSystem.
    #[must_use]
    pub fn new(
        agent_id: impl Into<String>,
        config: WorkConfig,
        local_bus: Arc<dyn EventBus>,
        system_state: Option<Arc<std::sync::Mutex<AgentSystemState>>>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            config,
            ctx: Mutex::new(WorkContext::new()),
            local_bus,
            idle_signal_tx: Mutex::new(None),
            system_state,
        }
    }

    /// Set the idle signal channel for feedback injection.
    pub async fn set_idle_signal_tx(&self, tx: tokio::sync::mpsc::UnboundedSender<IdleSignal>) {
        *self.idle_signal_tx.lock().await = Some(tx);
    }

    /// Returns the agent id.
    #[must_use]
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Returns the current work state.
    pub async fn current_state(&self) -> WorkState {
        self.ctx.lock().await.state
    }

    /// Returns a clone of the current work context.
    pub async fn snapshot(&self) -> WorkContext {
        self.ctx.lock().await.clone()
    }

    // ------------------------------------------------------------------
    // Main event handler
    // ------------------------------------------------------------------

    /// Handle a work event. This is the main entry point.
    ///
    /// The caller (agent event loop) deserializes incoming event payloads
    /// into [`WorkEvent`] and dispatches them here.
    pub async fn handle(&self, event: WorkEvent) -> WorkResult<()> {
        debug!(
            agent_id = %self.agent_id,
            event_kind = %event.kind(),
            "WorkSystem::handle"
        );

        match event {
            WorkEvent::Interrupt { reason, by_system } => {
                self.handle_interrupt(&reason, &by_system).await;
                Ok(())
            }
            WorkEvent::WorkItemAssigned { item, source } => {
                self.handle_work_item_assigned(item, source).await
            }
            WorkEvent::WorkItemCompleted { item_id, result, duration } => {
                self.handle_work_item_completed(item_id, result, duration).await
            }
            WorkEvent::WorkItemFailed { item_id, error, retryable } => {
                self.handle_work_item_failed(item_id, error, retryable).await
            }
        }
    }

    // ------------------------------------------------------------------
    // Handler implementations
    // ------------------------------------------------------------------

    async fn handle_interrupt(&self, reason: &str, by_system: &str) {
        let item_id;
        {
            let mut ctx = self.ctx.lock().await;
            let checkpoint = ctx.interrupt(reason);
            item_id = checkpoint.item_id;
            info!(
                agent_id = %self.agent_id,
                ?checkpoint,
                "WorkSystem interrupted by {by_system}: {reason}",
            );
        }

        self.transition_to(WorkState::Idle).await;

        self.record_trace(WorkTraceEvent::Interrupted {
            item_id,
            reason: reason.to_string(),
            by_system: by_system.to_string(),
        })
        .await;
    }

    async fn handle_work_item_assigned(
        &self,
        item: WorkItem,
        source: WorkItemSource,
    ) -> WorkResult<()> {
        let item_id = item.id;
        let source_str = source_name(&source);

        info!(
            agent_id = %self.agent_id,
            %item_id,
            title = %item.title,
            source = %source_str,
            "WorkItemAssigned — enqueuing"
        );

        self.record_trace(WorkTraceEvent::ItemReceived {
            item_id,
            source: source_str,
        })
        .await;

        let should_start;
        {
            let mut ctx = self.ctx.lock().await;
            ctx.enqueue(item);
            should_start = ctx.state == WorkState::Idle && ctx.current.is_none();
        }

        if should_start {
            self.start_item().await?;
        }

        Ok(())
    }

    async fn handle_work_item_completed(
        &self,
        item_id: WorkItemId,
        result: WorkItemResult,
        duration: std::time::Duration,
    ) -> WorkResult<()> {
        info!(
            agent_id = %self.agent_id,
            %item_id,
            steps_completed = %result.steps_completed,
            steps_failed = %result.steps_failed,
            "WorkItemCompleted"
        );

        self.record_trace(WorkTraceEvent::ItemCompleted {
            item_id,
            duration,
            steps_completed: result.steps_completed,
            steps_failed: result.steps_failed,
        })
        .await;

        self.send_idle_signal(IdleSignal::Satisfaction {
            work_item_id: item_id,
        })
        .await;

        self.process_next().await
    }

    async fn handle_work_item_failed(
        &self,
        item_id: WorkItemId,
        error: WorkError,
        retryable: bool,
    ) -> WorkResult<()> {
        warn!(
            agent_id = %self.agent_id,
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

        if retryable {
            // Re-enqueue the current item for retry.
            if let Some(current) = {
                let mut ctx = self.ctx.lock().await;
                ctx.current.take()
            } {
                let mut ctx = self.ctx.lock().await;
                ctx.enqueue(current);
            }
        }

        self.send_idle_signal(IdleSignal::Frustration {
            reason: Some(error.message),
        })
        .await;

        self.process_next().await
    }

    // ------------------------------------------------------------------
    // Item execution
    // ------------------------------------------------------------------

    /// Start executing the next item from the queue.
    async fn start_item(&self) -> WorkResult<()> {
        let item = {
            let mut ctx = self.ctx.lock().await;
            ctx.dequeue()
        };

        let item = match item {
            Some(i) => i,
            None => {
                // Queue is empty — go IDLE.
                self.transition_to(WorkState::Idle).await;
                return Ok(());
            }
        };

        let item_id = item.id;
        let steps = self.decompose_item(&item);

        {
            let mut ctx = self.ctx.lock().await;
            ctx.current = Some(item);
            ctx.steps = steps;
            ctx.step_index = 0;
            ctx.step_outputs.clear();
        }

        self.transition_to(WorkState::Busy).await;

        // Execute all steps sequentially.
        let start = Instant::now();
        let total_steps;
        let mut steps_completed: usize = 0;
        let mut steps_failed: usize = 0;
        let mut failed = false;

        {
            let ctx = self.ctx.lock().await;
            total_steps = ctx.steps.len();
        }

        for i in 0..total_steps {
            let step = {
                let ctx = self.ctx.lock().await;
                ctx.steps.get(i).cloned()
            };

            if let Some(step) = step {
                {
                    let mut ctx = self.ctx.lock().await;
                    ctx.step_index = i;
                }

                let step_start = Instant::now();
                match self.execute_step(&step, item_id).await {
                    Ok(output) => {
                        let step_duration = step_start.elapsed();
                        if output.success {
                            steps_completed += 1;
                        } else {
                            steps_failed += 1;
                        }
                        self.record_trace(WorkTraceEvent::StepExecuted {
                            item_id,
                            step_index: i,
                            duration: step_duration,
                            success: output.success,
                            error: None,
                        })
                        .await;
                        {
                            let mut ctx = self.ctx.lock().await;
                            ctx.step_outputs.push(output);
                        }
                    }
                    Err(error) => {
                        steps_failed += 1;
                        let step_duration = step_start.elapsed();
                        self.record_trace(WorkTraceEvent::StepExecuted {
                            item_id,
                            step_index: i,
                            duration: step_duration,
                            success: false,
                            error: Some(error.message.clone()),
                        })
                        .await;

                        if error.retryable && step.max_retries > 0 {
                            warn!(
                                agent_id = %self.agent_id,
                                %item_id,
                                step_index = i,
                                "Step failed (retryable): {}",
                                error.message,
                            );
                            // For now, treat retryable step failures as non-fatal
                            // and continue. Full retry logic is a future enhancement.
                        } else {
                            failed = true;
                            break;
                        }
                    }
                }
            }
        }

        let total_duration = start.elapsed();

        if failed {
            let error = WorkError {
                code: "step_execution_failed".into(),
                message: format!(
                    "{steps_failed}/{} step(s) failed",
                    total_steps
                ),
                retryable: true,
            };
            self.publish_work_event(WorkEvent::WorkItemFailed {
                item_id,
                error,
                retryable: true,
            })
            .await?;
        } else {
            let result = WorkItemResult {
                item_id,
                outcome: WorkOutcome::Completed,
                summary: format!(
                    "Completed {} steps ({steps_completed} ok, {steps_failed} failed)",
                    total_steps
                ),
                steps_completed,
                steps_failed,
                total_duration,
            };
            self.publish_work_event(WorkEvent::WorkItemCompleted {
                item_id,
                result,
                duration: total_duration,
            })
            .await?;
        }

        Ok(())
    }

    /// Process the next item in the queue, or go IDLE.
    async fn process_next(&self) -> WorkResult<()> {
        {
            let mut ctx = self.ctx.lock().await;
            ctx.current = None;
            ctx.steps.clear();
            ctx.step_index = 0;
            ctx.step_outputs.clear();
        }

        let has_next;
        {
            let ctx = self.ctx.lock().await;
            has_next = !ctx.queue.is_empty();
        }

        if has_next {
            self.start_item().await?;
        } else {
            self.transition_to(WorkState::Idle).await;
        }

        Ok(())
    }

    // ------------------------------------------------------------------
    // Decomposition
    // ------------------------------------------------------------------

    /// Decompose a work item into execution steps.
    fn decompose_item(&self, item: &WorkItem) -> Vec<crate::types::Step> {
        // If the item already has predefined steps, use them.
        if let Some(ref steps) = item.steps
            && !steps.is_empty()
        {
            return steps.clone();
        }

        // Otherwise produce a simple default plan.
        let mut steps = Vec::new();

        steps.push(crate::types::Step {
            index: 0,
            description: format!("Analyze: {}", item.title),
            tool: None,
            expect_llm: true,
            max_retries: 1,
        });

        let desc_lower = item.description.to_lowercase();
        if desc_lower.contains("code")
            || desc_lower.contains("fix")
            || desc_lower.contains("refactor")
            || desc_lower.contains("implement")
        {
            steps.push(crate::types::Step {
                index: steps.len(),
                description: format!("Implement: {}", item.title),
                tool: Some("file".into()),
                expect_llm: true,
                max_retries: 2,
            });
        }
        if desc_lower.contains("test") || desc_lower.contains("review") || desc_lower.contains("verify")
        {
            steps.push(crate::types::Step {
                index: steps.len(),
                description: format!("Verify: {}", item.title),
                tool: Some("exec".into()),
                expect_llm: false,
                max_retries: 1,
            });
        }

        steps.push(crate::types::Step {
            index: steps.len(),
            description: format!("Finalize: {}", item.title),
            tool: None,
            expect_llm: true,
            max_retries: 1,
        });

        steps
    }

    /// Execute a single step. Override this for actual LLM/tool integration.
    async fn execute_step(
        &self,
        step: &crate::types::Step,
        _item_id: WorkItemId,
    ) -> WorkResult<StepOutput> {
        debug!(
            agent_id = %self.agent_id,
            step_index = step.index,
            desc = %step.description,
            "Executing step"
        );
        // Placeholder: real integration calls LLM / runs tools here.
        Ok(StepOutput {
            success: true,
            summary: format!("Completed: {}", step.description),
            artifacts: Vec::new(),
            duration: std::time::Duration::from_millis(50),
        })
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
    // State transitions
    // ------------------------------------------------------------------

    async fn transition_to(&self, new_state: WorkState) {
        let mut ctx = self.ctx.lock().await;
        debug!(
            agent_id = %self.agent_id,
            from = ?ctx.state,
            to = ?new_state,
            "WorkSystem state transition",
        );
        ctx.state = new_state;
        drop(ctx);

        // Update shared system state for UI visibility.
        if let Some(ref ss) = self.system_state {
            let val = match new_state {
                WorkState::Idle => AgentSystemState::Idle,
                WorkState::Busy => AgentSystemState::Working,
            };
            *ss.lock().expect("system_state lock") = val;
        }
    }

    // ------------------------------------------------------------------
    // Idle signal injection
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
        debug!(agent_id = %self.agent_id, trace = ?event, "WorkTraceEvent");
    }

    // ------------------------------------------------------------------
    // Convenience: push work item from external source
    // ------------------------------------------------------------------

    /// Push a work item onto this agent's queue by publishing a
    /// [`WorkEvent::WorkItemAssigned`] on the local bus.
    ///
    /// External systems (CLI, API, kanban scheduler) call this to assign
    /// work without needing knowledge of the agent's internal queue.
    pub async fn push_work_item(
        &self,
        item: WorkItem,
        source: WorkItemSource,
    ) -> WorkResult<()> {
        self.publish_work_event(WorkEvent::WorkItemAssigned { item, source })
            .await
    }

    // ------------------------------------------------------------------
    // Shutdown
    // ------------------------------------------------------------------

    /// Gracefully shut down the work system: interrupt any in-progress
    /// work and clear pending state.
    pub async fn shutdown(&self) {
        info!(agent_id = %self.agent_id, "WorkSystem shutting down");
        self.handle_interrupt("shutdown", "core").await;
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
    use crate::types::{Priority, Step, WorkItemId};
    use event_bus::{EventBus, InMemoryBus, InMemoryBusConfig};
    use kernel::types::Timestamp;
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

    #[tokio::test]
    async fn system_starts_at_idle() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), None);
        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    #[tokio::test]
    async fn interrupt_from_any_state_goes_to_idle() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), None);
        // Manually set to Busy
        {
            let mut ctx = sys.ctx.lock().await;
            ctx.state = WorkState::Busy;
        }
        sys.handle(WorkEvent::Interrupt {
            reason: "test".into(),
            by_system: "test".into(),
        })
        .await
        .expect("handle should succeed");
        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    #[tokio::test]
    async fn work_item_assigned_enqueues_and_starts() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), None);
        let item = make_item("test-task");

        // Assign a work item with predefined steps so it completes instantly.
        let item_with_steps = WorkItem {
            steps: Some(vec![Step {
                index: 0,
                description: "do it".into(),
                tool: None,
                expect_llm: false,
                max_retries: 0,
            }]),
            ..item
        };

        // This will enqueue, start, execute steps, and publish WorkItemCompleted.
        // The published event goes to the in-memory bus but nobody dispatches
        // it back — so state after this is BUSY (last transition before publishing).
        sys.handle(WorkEvent::WorkItemAssigned {
            item: item_with_steps,
            source: WorkItemSource::Cli {
                operator: "user".into(),
            },
        })
        .await
        .expect("handle should succeed");

        // Queue should be empty (item was dequeued and started).
        let snap = sys.snapshot().await;
        assert!(snap.queue.is_empty());
    }

    #[tokio::test]
    async fn work_item_assigned_when_busy_just_enqueues() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), None);
        // Set to BUSY with a current item
        {
            let mut ctx = sys.ctx.lock().await;
            ctx.state = WorkState::Busy;
            ctx.current = Some(make_item("running"));
        }

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
        assert_eq!(snap.queue.len(), 1);
        assert_eq!(snap.state, WorkState::Busy);
    }

    #[tokio::test]
    async fn completed_item_with_empty_queue_goes_idle() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), None);
        let item_id = WorkItemId::new();

        sys.handle(WorkEvent::WorkItemCompleted {
            item_id,
            result: crate::types::WorkItemResult {
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
    async fn context_snapshot_reflects_current_state() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), None);
        let snap = sys.snapshot().await;
        assert_eq!(snap.state, WorkState::Idle);
        assert!(snap.queue.is_empty());
    }

    #[tokio::test]
    async fn push_work_item_publishes_assigned_event() {
        let bus = make_bus();
        let sys = WorkSystem::new("agent-1", make_config(), bus, None);
        let item = make_item("push-test");

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
    async fn failed_retryable_re_enqueues() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), None);
        let item_id = WorkItemId::new();

        // Put a current item so it gets re-enqueued on retryable failure,
        // but with predefined steps so it completes immediately.
        {
            let mut ctx = sys.ctx.lock().await;
            ctx.current = Some(WorkItem {
                steps: Some(vec![Step {
                    index: 0,
                    description: "retry-step".into(),
                    tool: None,
                    expect_llm: false,
                    max_retries: 0,
                }]),
                ..make_item("retry-me")
            });
        }

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
        // The item was re-enqueued, then immediately dequeued by process_next
        // and started, so queue is empty but state is BUSY.
        assert_eq!(snap.queue.len(), 0);
        assert!(snap.current.is_some());
        assert_eq!(snap.state, WorkState::Busy);
    }
}
