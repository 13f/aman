// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! WorkSystem — 核心状态机引擎。
//!
//! Architecture ref: work-design.md §4-5

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use event_bus::EventBus;
use kernel::agent::AgentSystemState;
use kernel::event::{Event, EventType};
use kernel::types::Timestamp;

use crate::personality::WorkPersonality;
use crate::trace::WorkTraceEvent;
use crate::types::{
    IdleSignal, Step, StepOutput, TaskBrief, TaskId, TaskResult, WorkContext,
    WorkEvent, WorkError, WorkOutcome, WorkResult, WorkState, WORK_SOURCE,
};

/// Trait for interacting with a task board (kanban/team).
///
/// Architecture ref: work-design.md §8.3
#[async_trait::async_trait]
pub trait WorkBoardClient: Send + Sync {
    /// Get available tasks filtered by agent capabilities.
    async fn get_available_tasks(&self, capabilities: &[String]) -> WorkResult<Vec<TaskBrief>>;

    /// Send a claim request (optimistic lock).
    async fn claim_task(&self, task_id: TaskId, agent_id: &str) -> WorkResult<bool>;

    /// Submit task result back to the board.
    async fn submit_result(&self, task_id: TaskId, result: &TaskResult) -> WorkResult<()>;
}

// ---------------------------------------------------------------------------
// WorkSystem
// ---------------------------------------------------------------------------

/// The per-agent Work System engine.
///
/// Each agent instance gets its own WorkSystem. The system is fully event-driven:
/// all state transitions happen in response to [`WorkEvent`] values dispatched
/// through [`handle`].
pub struct WorkSystem {
    /// This agent's identifier.
    agent_id: String,

    /// Work personality configuration.
    personality: WorkPersonality,

    /// Shared work context (state, current task, steps, etc.).
    ctx: Mutex<WorkContext>,

    /// Agent's local event bus.
    local_bus: Arc<dyn EventBus>,

    /// Task board client (kanban/team plugin).
    board: Option<Arc<dyn WorkBoardClient>>,

    /// Idle coordination: inject satisfaction/frustration.
    idle_signal_tx: Option<tokio::sync::mpsc::UnboundedSender<IdleSignal>>,

    /// Shared system state for UI visibility — set to Working/Idle on transitions.
    system_state: Option<Arc<std::sync::Mutex<AgentSystemState>>>,

    /// Pending delayed-work-tick handles so we can cancel on interrupt.
    delayed_tick_handles: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl WorkSystem {
    /// Create a new WorkSystem.
    #[must_use]
    pub fn new(
        agent_id: impl Into<String>,
        personality: WorkPersonality,
        local_bus: Arc<dyn EventBus>,
        board: Option<Arc<dyn WorkBoardClient>>,
        system_state: Option<Arc<std::sync::Mutex<AgentSystemState>>>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            personality,
            ctx: Mutex::new(WorkContext::new()),
            local_bus,
            board,
            idle_signal_tx: None,
            system_state,
            delayed_tick_handles: Mutex::new(Vec::new()),
        }
    }

    /// Set the idle signal channel for feedback injection.
    pub fn set_idle_signal_tx(&mut self, tx: tokio::sync::mpsc::UnboundedSender<IdleSignal>) {
        self.idle_signal_tx = Some(tx);
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
    // §5.2 — Main event handler
    // ------------------------------------------------------------------

    /// Handle a work event. This is the main state machine entry point.
    ///
    /// The caller (typically the agent's event loop) deserializes incoming
    /// event payloads into [`WorkEvent`] and dispatches them here.
    pub async fn handle(&self, event: WorkEvent) -> WorkResult<()> {
        let state = self.ctx.lock().await.state;
        debug!(
            agent_id = %self.agent_id,
            state = ?state,
            event_kind = %event.kind(),
            "WorkSystem::handle"
        );

        match (state, event.clone()) {
            // ── Interrupt (最高优先级，任何状态) ──────────────────
            (_, WorkEvent::Interrupt { reason, by_system }) => {
                self.handle_interrupt(&reason, &by_system).await;
                Ok(())
            }

            // ── IDLE ────────────────────────────────────────────
            (WorkState::Idle, _e @ WorkEvent::TaskBoardUpdated { .. }) => {
                self.transition_to(WorkState::Checking).await;
                self.publish_work_event(WorkEvent::StartCheck).await?;
                Ok(())
            }
            (WorkState::Idle, _e @ WorkEvent::WorkTick { .. })
            | (WorkState::Idle, _e @ WorkEvent::DelayedWorkTick { .. }) => {
                let (cooldown, last_check) = {
                    let ctx = self.ctx.lock().await;
                    (self.personality.work_cooldown, ctx.last_check_time)
                };
                let now = Timestamp::now();
                let elapsed_ms = (now.as_millis() - last_check.as_millis()).max(0) as u64;
                if Duration::from_millis(elapsed_ms) >= cooldown {
                    self.transition_to(WorkState::Checking).await;
                    self.publish_work_event(WorkEvent::StartCheck).await?;
                }
                Ok(())
            }
            (WorkState::Idle, _) => Ok(()),

            // ── CHECKING ─────────────────────────────────────────
            (WorkState::Checking, WorkEvent::StartCheck) => {
                self.handle_start_check().await
            }

            // ── CLAIMING ─────────────────────────────────────────
            (WorkState::Claiming, WorkEvent::ClaimTask(task)) => {
                self.handle_claim_task(task).await
            }
            (WorkState::Claiming, WorkEvent::ClaimResponse {
                task,
                success,
                reason,
            }) => {
                self.handle_claim_response(task, success, reason).await
            }

            // ── EXECUTING ────────────────────────────────────────
            (WorkState::Executing, WorkEvent::ExecuteStep {
                task_id,
                step_index,
            }) => {
                self.handle_execute_step(task_id, step_index).await
            }

            // ── REVIEWING ────────────────────────────────────────
            (WorkState::Reviewing, WorkEvent::ReviewTask(task)) => {
                self.handle_review_task(task).await
            }

            // ── Invalid transition ───────────────────────────────
            _ => {
                warn!(
                    agent_id = %self.agent_id,
                    "WorkSystem: invalid transition {:?} + {:?}",
                    state,
                    event.kind(),
                );
                Ok(())
            }
        }
    }

    // ------------------------------------------------------------------
    // Handler implementations
    // ------------------------------------------------------------------

    async fn handle_interrupt(&self, reason: &str, by_system: &str) {
        // Cancel all pending delayed ticks
        self.cancel_delayed_ticks().await;

        let task_id;
        {
            let mut ctx = self.ctx.lock().await;
            let checkpoint = ctx.interrupt(reason);
            task_id = checkpoint.task_id;
            info!(
                agent_id = %self.agent_id,
                ?checkpoint,
                "WorkSystem interrupted by {by_system}: {reason}",
            );
        }

        // Record in trace
        self.record_trace(WorkTraceEvent::Interrupted {
            task_id,
            reason: reason.to_string(),
            by_system: by_system.to_string(),
        })
        .await;
    }

    async fn handle_start_check(&self) -> WorkResult<()> {
        {
            let mut ctx = self.ctx.lock().await;
            ctx.last_check_time = Timestamp::now();
        }

        let board = match &self.board {
            Some(b) => b.clone(),
            None => {
                debug!(agent_id = %self.agent_id, "No board configured, returning to IDLE");
                self.schedule_next_check().await;
                self.transition_to(WorkState::Idle).await;
                return Ok(());
            }
        };

        let tasks = board
            .get_available_tasks(&self.personality.capabilities)
            .await
            .unwrap_or_default();

        self.record_trace(WorkTraceEvent::CheckStarted {
            candidates_count: tasks.len(),
        })
        .await;

        if tasks.is_empty() {
            debug!(agent_id = %self.agent_id, "No available tasks, returning to IDLE");
            self.schedule_next_check().await;
            self.transition_to(WorkState::Idle).await;
            return Ok(());
        }

        let best = self
            .personality
            .selection
            .select(&tasks, &self.personality.capabilities);

        match best {
            Some(task) => {
                let task_id = task.id;
                debug!(agent_id = %self.agent_id, %task_id, title = %task.title, "Selected task");
                {
                    let mut ctx = self.ctx.lock().await;
                    ctx.state = WorkState::Claiming;
                }
                self.publish_work_event(WorkEvent::ClaimTask(task)).await?;
                Ok(())
            }
            None => {
                self.schedule_next_check().await;
                self.transition_to(WorkState::Idle).await;
                Ok(())
            }
        }
    }

    async fn handle_claim_task(&self, task: TaskBrief) -> WorkResult<()> {
        let task_id = task.id;
        let board = match &self.board {
            Some(b) => b.clone(),
            None => {
                warn!(agent_id = %self.agent_id, "No board configured for claim");
                self.fail_claim(task_id, Some("no board configured".into()))
                    .await;
                return Ok(());
            }
        };

        match board.claim_task(task_id, &self.agent_id).await {
            Ok(true) => {
                // Success — publish ClaimResponse back to self
                self.publish_work_event(WorkEvent::ClaimResponse {
                    task: task.clone(),
                    success: true,
                    reason: None,
                })
                .await?;
            }
            Ok(false) => {
                self.publish_work_event(WorkEvent::ClaimResponse {
                    task: task.clone(),
                    success: false,
                    reason: Some("task_taken_by_other".into()),
                })
                .await?;
            }
            Err(e) => {
                self.publish_work_event(WorkEvent::ClaimResponse {
                    task,
                    success: false,
                    reason: Some(e.message),
                })
                .await?;
            }
        }

        Ok(())
    }

    async fn handle_claim_response(
        &self,
        task: TaskBrief,
        success: bool,
        reason: Option<String>,
    ) -> WorkResult<()> {
        if success {
            let steps = self.decompose_task(&task);
            {
                let mut ctx = self.ctx.lock().await;
                ctx.current_task = Some(task.clone());
                ctx.task_steps = steps;
                ctx.step_index = 0;
                ctx.consecutive_claim_failures = 0;
                ctx.state = WorkState::Executing;
            }
            self.record_trace(WorkTraceEvent::ClaimAttempted {
                task_id: task.id,
                outcome: crate::trace::ClaimOutcome::Success,
            })
            .await;
            self.publish_work_event(WorkEvent::ExecuteStep {
                task_id: task.id,
                step_index: 0,
            })
            .await?;
        } else {
            {
                let mut ctx = self.ctx.lock().await;
                ctx.consecutive_claim_failures += 1;
            }
            self.record_trace(WorkTraceEvent::ClaimAttempted {
                task_id: task.id,
                outcome: crate::trace::ClaimOutcome::TaskTakenByOther,
            })
            .await;
            self.send_idle_signal(IdleSignal::Frustration { reason });
            self.fail_claim(task.id, None).await;
        }
        Ok(())
    }

    async fn handle_execute_step(&self, task_id: TaskId, step_index: usize) -> WorkResult<()> {
        let step = {
            let ctx = self.ctx.lock().await;
            ctx.task_steps.get(step_index).cloned()
        };

        let step = match step {
            Some(s) => s,
            None => {
                warn!(agent_id = %self.agent_id, %task_id, step_index, "Step out of range");
                // All steps done — move to review
                self.transition_to(WorkState::Reviewing).await;
                let task = {
                    let ctx = self.ctx.lock().await;
                    ctx.current_task.clone()
                };
                if let Some(t) = task {
                    self.publish_work_event(WorkEvent::ReviewTask(t)).await?;
                }
                return Ok(());
            }
        };

        // Execute the step
        let start = std::time::Instant::now();
        let result = self.execute_step(&step, task_id).await;
        let duration = start.elapsed();

        match result {
            Ok(output) => {
                {
                    let mut ctx = self.ctx.lock().await;
                    ctx.step_outputs.push(output.clone());
                }
                self.record_trace(WorkTraceEvent::StepExecuted {
                    task_id,
                    step_index,
                    duration,
                    success: true,
                    error: None,
                })
                .await;

                let total_steps = {
                    let ctx = self.ctx.lock().await;
                    ctx.task_steps.len()
                };

                if step_index + 1 < total_steps {
                    // Chain to next step — keeps bus non-empty
                    self.publish_work_event(WorkEvent::ExecuteStep {
                        task_id,
                        step_index: step_index + 1,
                    })
                    .await?;
                } else {
                    // All steps complete — move to review
                    self.transition_to(WorkState::Reviewing).await;
                    let task = {
                        let ctx = self.ctx.lock().await;
                        ctx.current_task.clone()
                    };
                    if let Some(t) = task {
                        self.publish_work_event(WorkEvent::ReviewTask(t)).await?;
                    }
                }
            }
            Err(error) => {
                self.record_trace(WorkTraceEvent::StepExecuted {
                    task_id,
                    step_index,
                    duration,
                    success: false,
                    error: Some(error.message.clone()),
                })
                .await;

                if error.retryable && self.should_retry_step(step_index) {
                    warn!(agent_id = %self.agent_id, %task_id, step_index, "Retrying step: {}", error.message);
                    self.publish_work_event(WorkEvent::ExecuteStep {
                        task_id,
                        step_index,
                    })
                    .await?;
                } else {
                    // Unrecoverable — abandon and go to IDLE
                    let total_steps = {
                        let ctx = self.ctx.lock().await;
                        ctx.task_steps.len()
                    };
                    self.send_idle_signal(IdleSignal::Disappointment { task_id });
                    self.transition_to(WorkState::Idle).await;
                    self.schedule_next_check().await;
                    self.record_trace(WorkTraceEvent::CycleCompleted {
                        task_id,
                        outcome: WorkOutcome::Failed { retryable: false },
                        total_duration: Duration::ZERO,
                        steps_completed: step_index,
                        steps_failed: total_steps.saturating_sub(step_index),
                    })
                    .await;
                }
            }
        }
        Ok(())
    }

    async fn handle_review_task(&self, task: TaskBrief) -> WorkResult<()> {
        let task_id = task.id;
        let passed = self.verify_result(&task).await;

        self.record_trace(WorkTraceEvent::ReviewCompleted {
            task_id,
            passed,
            confidence: if passed { 0.9 } else { 0.3 },
        })
        .await;

        let steps_completed = {
            let ctx = self.ctx.lock().await;
            ctx.step_outputs
                .iter()
                .filter(|o| o.success)
                .count()
        };
        let steps_failed = {
            let ctx = self.ctx.lock().await;
            ctx.step_outputs.iter().filter(|o| !o.success).count()
        };

        let outcome = if passed {
            WorkOutcome::Completed
        } else {
            WorkOutcome::Failed { retryable: true }
        };

        // Submit result to board
        if let Some(board) = &self.board {
            let result = TaskResult {
                task_id,
                outcome: outcome.clone(),
                summary: if passed {
                    "review passed".into()
                } else {
                    "review failed".into()
                },
                steps_completed,
                steps_failed,
                total_duration: Duration::ZERO,
            };
            let _ = board.submit_result(task_id, &result).await;
        }

        // Inject feedback into idle system
        if passed {
            self.send_idle_signal(IdleSignal::Satisfaction { task_id });
        } else {
            self.send_idle_signal(IdleSignal::Disappointment { task_id });
        }

        self.record_trace(WorkTraceEvent::CycleCompleted {
            task_id,
            outcome,
            total_duration: Duration::ZERO,
            steps_completed,
            steps_failed,
        })
        .await;

        {
            let mut ctx = self.ctx.lock().await;
            ctx.reset_to_idle();
        }
        self.schedule_next_check().await;
        Ok(())
    }

    // ------------------------------------------------------------------
    // §5.2 helpers — claim failure & retry backoff
    // ------------------------------------------------------------------

    async fn fail_claim(&self, _task_id: TaskId, reason: Option<String>) {
        let consecutive;
        {
            let ctx = self.ctx.lock().await;
            consecutive = ctx.consecutive_claim_failures;
        }

        self.send_idle_signal(IdleSignal::Frustration { reason });
        self.transition_to(WorkState::Idle).await;

        if self.personality.claim_retry.is_exhausted(consecutive) {
            info!(
                agent_id = %self.agent_id,
                "Claim retries exhausted ({consecutive}), abandoning work cycle"
            );
            return;
        }

        let delay = self.personality.claim_retry.backoff_delay(consecutive);
        self.schedule_delayed_tick(delay, format!("claim retry #{consecutive}")).await;
    }

    // ------------------------------------------------------------------
    // Decomposition
    // ------------------------------------------------------------------

    fn decompose_task(&self, task: &TaskBrief) -> Vec<Step> {
        // Simple decomposition: produce a sequence of steps based on the
        // task description and personality's decomposition strategy.
        let mut steps = Vec::new();

        // Step 0: understand the task
        steps.push(Step {
            index: 0,
            description: format!("Analyze task: {}", task.title),
            requires_llm: true,
            requires_tool: false,
            estimated_duration: Duration::from_secs(15),
        });

        // Step 1-N: execute based on task description
        let desc_lower = task.description.to_lowercase();
        if desc_lower.contains("code") || desc_lower.contains("fix") || desc_lower.contains("refactor") {
            steps.push(Step {
                index: steps.len(),
                description: format!("Implement changes for: {}", task.title),
                requires_llm: self.personality.decomposition.isolate_llm_calls,
                requires_tool: self.personality.decomposition.isolate_tool_calls,
                estimated_duration: self.personality.decomposition.max_step_duration,
            });
        }
        if desc_lower.contains("test") || desc_lower.contains("review") {
            steps.push(Step {
                index: steps.len(),
                description: format!("Run tests/validation for: {}", task.title),
                requires_llm: false,
                requires_tool: true,
                estimated_duration: Duration::from_secs(60),
            });
        }

        // Final step: collect outputs and prepare for review
        steps.push(Step {
            index: steps.len(),
            description: format!("Prepare review summary for: {}", task.title),
            requires_llm: true,
            requires_tool: false,
            estimated_duration: Duration::from_secs(10),
        });

        steps
    }

    /// Execute a single step. Override this for actual LLM/tool integration.
    async fn execute_step(&self, step: &Step, _task_id: TaskId) -> WorkResult<StepOutput> {
        debug!(agent_id = %self.agent_id, step_index = step.index, desc = %step.description, "Executing step");
        // Placeholder: real integration would call LLM / run tools here.
        Ok(StepOutput {
            success: true,
            summary: format!("Completed: {}", step.description),
            artifacts: Vec::new(),
            duration: Duration::from_millis(50),
        })
    }

    /// Verify/validate the result of a completed task.
    async fn verify_result(&self, _task: &TaskBrief) -> bool {
        // Placeholder: real integration would run automated checks here.
        true
    }

    fn should_retry_step(&self, _step_index: usize) -> bool {
        // Simple heuristic: retry once per step by default.
        true
    }

    // ------------------------------------------------------------------
    // Event publishing helpers
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

    async fn schedule_delayed_tick(&self, delay: Duration, reason: String) {
        let bus = self.local_bus.clone();
        let now = Timestamp::now();
        let fire_at = Timestamp::from_millis(now.as_millis() + delay.as_millis() as i64);
        let agent_id = self.agent_id.clone();

        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let event = WorkEvent::DelayedWorkTick {
                fire_at,
                reason,
            };
            let payload = serde_json::to_value(&event).unwrap_or_default();
            let ev = Event::new(WORK_SOURCE, EventType::Custom(event.kind().to_string()), payload);
            let _ = bus.publish(ev).await;
            debug!(agent_id = %agent_id, "DelayedWorkTick fired");
        });

        self.delayed_tick_handles.lock().await.push(handle);
    }

    async fn cancel_delayed_ticks(&self) {
        let handles: Vec<_> = {
            let mut guard = self.delayed_tick_handles.lock().await;
            std::mem::take(&mut *guard)
        };
        for h in handles {
            h.abort();
        }
    }

    async fn schedule_next_check(&self) {
        if self.personality.auto_claim {
            self.schedule_delayed_tick(
                self.personality.work_cooldown,
                "scheduled next check".into(),
            )
            .await;
        }
    }

    async fn transition_to(&self, new_state: WorkState) {
        let mut ctx = self.ctx.lock().await;
        debug!(
            agent_id = %self.agent_id,
            from = ?ctx.state,
            to = ?new_state,
            "WorkSystem state transition",
        );
        ctx.state = new_state;
        // Update shared system state for UI visibility
        if let Some(ref ss) = self.system_state {
            let val = match new_state {
                WorkState::Idle => AgentSystemState::Idle,
                _ => AgentSystemState::Working,
            };
            *ss.lock().expect("system_state lock") = val;
        }
    }

    // ------------------------------------------------------------------
    // Idle signal injection
    // ------------------------------------------------------------------

    fn send_idle_signal(&self, signal: IdleSignal) {
        if let Some(tx) = &self.idle_signal_tx {
            let _ = tx.send(signal);
        }
    }

    // ------------------------------------------------------------------
    // Trace recording
    // ------------------------------------------------------------------

    async fn record_trace(&self, _event: WorkTraceEvent) {
        // Placeholder: record to trace store.
        // Real implementation would call trace_store methods.
        debug!(agent_id = %self.agent_id, trace = ?_event, "WorkTraceEvent");
    }

    // ------------------------------------------------------------------
    // Shutdown
    // ------------------------------------------------------------------

    /// Gracefully shut down the work system: cancel pending ticks,
    /// flush any in-progress state.
    pub async fn shutdown(&self) {
        info!(agent_id = %self.agent_id, "WorkSystem shutting down");
        self.cancel_delayed_ticks().await;
        // Interrupt any in-progress work
        self.handle_interrupt("shutdown", "core").await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::personality::RetryStrategy;
    use crate::personality::TaskSelectionStrategy;
use crate::types::TaskBoardChangeType;
use event_bus::{EventBus, InMemoryBus, InMemoryBusConfig};
    use std::sync::Arc;

    fn make_bus() -> Arc<dyn EventBus> {
        Arc::new(InMemoryBus::new(InMemoryBusConfig::default()))
    }

    fn make_personality() -> WorkPersonality {
        WorkPersonality {
            auto_claim: true,
            capabilities: vec!["code".into()],
            max_concurrent: 1,
            work_cooldown: Duration::from_secs(60),
            claim_retry: RetryStrategy {
                base_delay: Duration::from_secs(1),
                backoff_multiplier: 2.0,
                max_delay: Duration::from_secs(60),
                max_consecutive_failures: 3,
            },
            selection: TaskSelectionStrategy::EarliestFirst,
            decomposition: Default::default(),
        }
    }

    #[tokio::test]
    async fn system_starts_at_idle() {
        let sys = WorkSystem::new("agent-1", make_personality(), make_bus(), None, None);
        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    #[tokio::test]
    async fn interrupt_from_any_state_goes_to_idle() {
        let sys = WorkSystem::new("agent-1", make_personality(), make_bus(), None, None);
        // Manually set to executing
        {
            let mut ctx = sys.ctx.lock().await;
            ctx.state = WorkState::Executing;
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
    async fn idle_ignores_unrelated_events() {
        let sys = WorkSystem::new("agent-1", make_personality(), make_bus(), None, None);
        let result = sys
            .handle(WorkEvent::ExecuteStep {
                task_id: TaskId::new(),
                step_index: 0,
            })
            .await;
        assert!(result.is_ok());
        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    #[tokio::test]
    async fn task_board_updated_triggers_start_check() {
        let bus = make_bus();
        let sys = WorkSystem::new("agent-1", make_personality(), bus.clone(), None, None);
        sys.handle(WorkEvent::TaskBoardUpdated {
            board_id: "kb-1".into(),
            change_type: TaskBoardChangeType::TaskAdded,
        })
        .await
        .expect("handle should succeed");
        assert_eq!(sys.current_state().await, WorkState::Checking);
    }

    #[tokio::test]
    async fn work_tick_during_cooldown_is_ignored() {
        let sys = WorkSystem::new("agent-1", make_personality(), make_bus(), None, None);
        // Set last_check_time to now, so cooldown hasn't elapsed
        {
            let mut ctx = sys.ctx.lock().await;
            ctx.last_check_time = Timestamp::now();
        }
        sys.handle(WorkEvent::WorkTick {
            triggered_by: "test".into(),
        })
        .await
        .expect("should be ok");
        // Should stay idle because cooldown hasn't passed
        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    #[tokio::test]
    async fn context_snapshot_reflects_current_state() {
        let sys = WorkSystem::new("agent-1", make_personality(), make_bus(), None, None);
        let snap = sys.snapshot().await;
        assert_eq!(snap.state, WorkState::Idle);
        assert_eq!(snap.consecutive_claim_failures, 0);
    }
}
