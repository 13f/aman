// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! WorkSystem — passive FIFO queue consumer engine.
//!
//! Architecture ref: work-design.md v2 §4-5.
//!
//! Key design:
//! - External systems push [`WorkEvent::WorkItemAssigned`] onto the local bus.
//! - `handle()` receives WorkEvents; `handle_step()` receives internal StepEvents.
//! - Step execution is event-driven: each step posts the next StepEvent to the bus,
//!   keeping it non-empty so the Idle System stays suppressed (§4.3).
//! - Hooks are **not executed by Rust**. An external script subscribes to bus events
//!   and runs hooks defined in [`WorkConfig::hooks`] out-of-process.

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
    IdleSignal, Step, StepEvent, StepOutput, WorkContext, WorkError, WorkEvent, WorkItem,
    WorkItemFailedEvent, WorkItemId, WorkItemResult, WorkItemResultEvent, WorkItemSource,
    WorkOutcome, WorkResult, WorkState, WORK_SOURCE, WORK_STEP_KIND,
};

// ---------------------------------------------------------------------------
// WorkSystem
// ---------------------------------------------------------------------------

/// The per-agent Work System engine (v2: passive queue consumer).
///
/// External systems push [`WorkEvent::WorkItemAssigned`] onto the event bus;
/// the Work System consumes them via [`handle`](Self::handle). No polling,
/// no claim competition, no board client — a pure FIFO consumer with
/// event-driven step execution.
pub struct WorkSystem {
    /// This agent's identifier.
    agent_id: String,

    /// Work configuration.
    config: WorkConfig,

    /// Shared work context (state, queue, current item, steps).
    ctx: Mutex<WorkContext>,

    /// Agent's local event bus (intra-agent).
    local_bus: Arc<dyn EventBus>,

    /// Global event bus (inter-agent notifications).
    global_bus: Arc<dyn EventBus>,

    /// Channel to inject satisfaction/frustration signals into the Idle System.
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
        global_bus: Arc<dyn EventBus>,
        system_state: Option<Arc<std::sync::Mutex<AgentSystemState>>>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            config,
            ctx: Mutex::new(WorkContext::new()),
            local_bus,
            global_bus,
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
    // §4.1 — Main event handler
    // ------------------------------------------------------------------

    /// Handle a [`WorkEvent`]. This is the main entry point called by the
    /// agent event loop when a routed work event arrives on the local bus.
    pub async fn handle(&self, event: WorkEvent) -> WorkResult<()> {
        debug!(
            "WorkSystem::handle agent_id={} event_kind={}",
            self.agent_id,
            event.kind(),
        );

        match event {
            // ── Interrupt（最高优先级，任何状态）────────────────
            // §4.1: save checkpoint → unconditional IDLE.
            WorkEvent::Interrupt { reason, by_system } => {
                let item_id;
                {
                    let mut ctx = self.ctx.lock().await;
                    let checkpoint = ctx.interrupt(&reason);
                    item_id = checkpoint.item_id;
                    info!(
                        "WorkSystem interrupted by {by_system}: {reason} agent_id={} checkpoint={checkpoint:?}",
                        self.agent_id,
                    );
                }
                self.transition_to(WorkState::Idle).await;
                self.record_trace(WorkTraceEvent::Interrupted {
                    item_id,
                    reason,
                    by_system,
                })
                .await;
                Ok(())
            }

            // ── 收到新工作项 ──────────────────────────────────
            // §4.1: enqueue; if IDLE → BUSY → dequeue → start_item.
            WorkEvent::WorkItemAssigned { item, source } => {
                let item_id = item.id;
                let source_str = source_name(&source);

                info!(
                    "WorkItemAssigned — enqueuing agent_id={} item_id={} title={} source={}",
                    self.agent_id, item_id, item.title, source_str,
                );

                self.record_trace(WorkTraceEvent::ItemReceived {
                    item_id,
                    source: source_str,
                })
                .await;

                let should_start;
                {
                    let mut ctx = self.ctx.lock().await;

                    // Queue size check
                    if ctx.queue.len() >= self.config.queue.max_size {
                        return Err(WorkError {
                            code: "queue_full".into(),
                            message: format!(
                                "Queue at capacity ({})",
                                self.config.queue.max_size
                            ),
                            retryable: false,
                        });
                    }

                    ctx.enqueue(item);
                    should_start = ctx.state == WorkState::Idle;
                }

                if should_start {
                    self.transition_to(WorkState::Busy).await;
                    self.start_item().await?;
                }
                // else: BUSY, item stays queued, process_next picks it up.

                Ok(())
            }

            // ── 工作项完成 ────────────────────────────────────
            // §4.1: trace, notify global bus if requested, IdleSignal, process_next.
            WorkEvent::WorkItemCompleted {
                item_id,
                result,
                duration,
            } => {
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

                // Notify global bus if the item requested it.
                {
                    let ctx = self.ctx.lock().await;
                    if let Some(ref item) = ctx.current {
                        if item.notify_on_complete {
                            let _ = self
                                .global_bus
                                .publish(Event::new(
                                    WORK_SOURCE,
                                    EventType::Custom("work.item.result".into()),
                                    serde_json::to_value(WorkItemResultEvent {
                                        item_id,
                                        result: result.clone(),
                                        agent_id: self.agent_id.clone(),
                                    })
                                    .unwrap_or_default(),
                                ))
                                .await;
                        }
                    }
                }

                self.send_idle_signal(IdleSignal::Satisfaction {
                    work_item_id: item_id,
                })
                .await;

                self.process_next().await
            }

            // ── 工作项失败 ────────────────────────────────────
            // §4.1: trace; if retryable && should_retry → push_front;
            //       else → global_bus notify + Frustration signal.
            //       Then process_next.
            WorkEvent::WorkItemFailed {
                item_id,
                error,
                retryable,
            } => {
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

                if retryable && self.should_retry(&error) {
                    // Re-enqueue to front for priority retry.
                    if let Some(current) = {
                        let mut ctx = self.ctx.lock().await;
                        ctx.current.take()
                    } {
                        let mut ctx = self.ctx.lock().await;
                        ctx.queue.push_front(current);
                    }
                } else {
                    // Non-retryable: notify global bus, signal frustration.
                    let _ = self
                        .global_bus
                        .publish(Event::new(
                            WORK_SOURCE,
                            EventType::Custom("work.item.failed_event".into()),
                            serde_json::to_value(WorkItemFailedEvent {
                                item_id,
                                error: error.message.clone(),
                                agent_id: self.agent_id.clone(),
                            })
                            .unwrap_or_default(),
                        ))
                        .await;

                    self.send_idle_signal(IdleSignal::Frustration {
                        reason: Some(error.message),
                    })
                    .await;
                }

                self.process_next().await
            }
        }
    }

    // ------------------------------------------------------------------
    // §4.2 — Step execution entry point (called when StepEvent arrives on bus)
    // ------------------------------------------------------------------

    /// Handle an internal [`StepEvent`] from the local bus.
    ///
    /// This is the event-driven step dispatch: executes one step, then
    /// posts either the next StepEvent or the terminal WorkEvent.
    /// Called by the agent event loop when it sees a `work.step.execute` event.
    pub async fn handle_step(&self, step_index: usize) -> WorkResult<()> {
        debug!(
            agent_id = %self.agent_id,
            step_index,
            "WorkSystem::handle_step"
        );
        self.execute_step(step_index).await
    }

    // ------------------------------------------------------------------
    // §5 — Convenience: push work item from external source
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

    /// Gracefully shut down: interrupt any in-progress work, clear state.
    pub async fn shutdown(&self) {
        info!("WorkSystem shutting down agent_id={}", self.agent_id);
        let event = WorkEvent::Interrupt {
            reason: "shutdown".into(),
            by_system: "core".into(),
        };
        let _ = self.handle(event).await;
    }

    // ------------------------------------------------------------------
    // §4.1 — start_item: begin executing the next queued item
    // ------------------------------------------------------------------

    /// Dequeue the next item, decompose steps, and post the first
    /// [`StepEvent`] to the local bus. The bus stays non-empty while
    /// steps chain through `execute_step`.
    async fn start_item(&self) -> WorkResult<()> {
        let item = {
            let mut ctx = self.ctx.lock().await;
            ctx.dequeue()
        };

        let item = match item {
            Some(i) => i,
            None => {
                // Queue emptied between check and dequeue — go IDLE.
                self.transition_to(WorkState::Idle).await;
                return Ok(());
            }
        };

        let item_id = item.id;

        // Decompose into steps (predefined or LLM).
        let steps = match item.steps.clone() {
            Some(predefined) if !predefined.is_empty() => predefined,
            _ => {
                if self.config.execution.auto_decompose {
                    self.decompose_with_llm(&item).await
                } else {
                    // Single default step: think → act.
                    vec![Step {
                        index: 0,
                        description: format!("Execute: {}", item.title),
                        tool: None,
                        expect_llm: true,
                        max_retries: self.config.retry.max_step_retries,
                    }]
                }
            }
        };

        {
            let mut ctx = self.ctx.lock().await;
            ctx.current = Some(item);
            ctx.steps = steps;
            ctx.step_index = 0;
            ctx.step_outputs.clear();
        }

        let total_steps = self.ctx.lock().await.steps.len();
        info!(
            "WorkSystem starting item agent_id={} item_id={item_id} total_steps={total_steps}",
            self.agent_id,
        );

        // Post first step event → bus becomes non-empty, Idle suppressed.
        self.publish_step_event(0).await
    }

    // ------------------------------------------------------------------
    // §4.2 — execute_step: run one step, post next event
    // ------------------------------------------------------------------

    /// Execute a single step and post the next event (next StepEvent or
    /// terminal WorkEvent). This is the core of the event-driven chain.
    async fn execute_step(&self, step_index: usize) -> WorkResult<()> {
        let step = {
            let ctx = self.ctx.lock().await;
            ctx.steps.get(step_index).cloned()
        };

        let step = match step {
            Some(s) => s,
            None => {
                warn!(
                    agent_id = %self.agent_id,
                    step_index,
                    "Step index out of range — completing item",
                );
                // Out-of-range: post completion to clean up.
                return self.finish_item().await;
            }
        };

        let item_id = {
            let ctx = self.ctx.lock().await;
            ctx.current.as_ref().map(|i| i.id)
        };

        let item_id = match item_id {
            Some(id) => id,
            None => {
                // No current item — should not happen, go IDLE.
                self.transition_to(WorkState::Idle).await;
                return Ok(());
            }
        };

        debug!(
            agent_id = %self.agent_id,
            %item_id,
            step_index,
            desc = %step.description,
            "Executing step"
        );

        // ---- Execute the step (no lock held) ----
        let step_start = Instant::now();

        let result = if step.expect_llm {
            self.execute_llm_step(&step).await
        } else if step.tool.is_some() {
            self.execute_tool_step(&step).await
        } else {
            self.execute_simple_step(&step).await
        };

        let step_duration = step_start.elapsed();

        // ---- Process result ----
        match result {
            Ok(output) => {
                let has_more;
                {
                    let mut ctx = self.ctx.lock().await;
                    ctx.step_outputs.push(StepOutput {
                        duration: step_duration,
                        ..output
                    });
                    has_more = step_index + 1 < ctx.steps.len();
                    if has_more {
                        ctx.step_index = step_index + 1;
                    }
                }

                self.record_trace(WorkTraceEvent::StepExecuted {
                    item_id,
                    step_index,
                    duration: step_duration,
                    success: true,
                    error: None,
                })
                .await;

                if has_more {
                    // Chain to next step → bus stays non-empty.
                    self.publish_step_event(step_index + 1).await?;
                } else {
                    // All steps done — post terminal completion.
                    self.finish_item().await?;
                }
            }
            Err(error) => {
                self.record_trace(WorkTraceEvent::StepExecuted {
                    item_id,
                    step_index,
                    duration: step_duration,
                    success: false,
                    error: Some(error.message.clone()),
                })
                .await;

                if error.retryable && step.max_retries > 0 {
                    warn!(
                        agent_id = %self.agent_id,
                        %item_id,
                        step_index,
                        "Step failed (retryable): {}",
                        error.message,
                    );
                    // Retry the same step.
                    self.publish_step_event(step_index).await?;
                } else {
                    // Terminal failure for this work item.
                    self.publish_work_event(WorkEvent::WorkItemFailed {
                        item_id,
                        error,
                        retryable: false,
                    })
                    .await?;
                }
            }
        }

        Ok(())
    }

    // ------------------------------------------------------------------
    // finish_item: collect result and post WorkItemCompleted
    // ------------------------------------------------------------------

    async fn finish_item(&self) -> WorkResult<()> {
        let (item_id, result, total_duration) = {
            let ctx = self.ctx.lock().await;
            let item = match ctx.current.as_ref() {
                Some(i) => i,
                None => {
                    // No current item — nothing to finish.
                    self.transition_to(WorkState::Idle).await;
                    return Ok(());
                }
            };
            let item_id = item.id;
            let result = collect_result(&ctx);
            let total_duration: std::time::Duration =
                ctx.step_outputs.iter().map(|o| o.duration).sum();
            (item_id, result, total_duration)
        };

        info!(
            agent_id = %self.agent_id,
            %item_id,
            steps_completed = %result.steps_completed,
            steps_failed = %result.steps_failed,
            total_duration_ms = total_duration.as_millis(),
            "WorkItem finished — posting completion"
        );

        self.publish_work_event(WorkEvent::WorkItemCompleted {
            item_id,
            result,
            duration: total_duration,
        })
        .await
    }

    // ------------------------------------------------------------------
    // §4.1 — process_next: dequeue next item or go IDLE
    // ------------------------------------------------------------------

    /// Clear current item context. If the queue has another item, start it.
    /// Otherwise transition to IDLE.
    async fn process_next(&self) -> WorkResult<()> {
        let has_next;
        {
            let mut ctx = self.ctx.lock().await;
            ctx.current = None;
            ctx.steps.clear();
            ctx.step_index = 0;
            ctx.step_outputs.clear();
            has_next = !ctx.queue.is_empty();
        }

        if has_next {
            // Stay BUSY, start next item.
            self.start_item().await?;
        } else {
            // Queue empty → IDLE, bus becomes empty, Idle System resumes.
            self.transition_to(WorkState::Idle).await;
        }

        Ok(())
    }

    // ------------------------------------------------------------------
    // §4.2 — Step execution variants
    // ------------------------------------------------------------------

    /// Execute a step that requires LLM reasoning.
    async fn execute_llm_step(&self, step: &Step) -> WorkResult<StepOutput> {
        // Placeholder: real integration calls LLM with step description
        // and work item context, parses the response.
        let _ = step;
        Ok(StepOutput {
            success: true,
            summary: format!("LLM reasoning completed: {}", step.description),
            artifacts: Vec::new(),
            duration: std::time::Duration::from_millis(50),
        })
    }

    /// Execute a step using a specific tool.
    async fn execute_tool_step(&self, step: &Step) -> WorkResult<StepOutput> {
        // Placeholder: real integration looks up the tool by name and
        // invokes it with the work item context.
        let _ = step;
        Ok(StepOutput {
            success: true,
            summary: format!("Tool execution completed: {}", step.description),
            artifacts: Vec::new(),
            duration: std::time::Duration::from_millis(50),
        })
    }

    /// Execute a simple step (no LLM, no tool).
    async fn execute_simple_step(&self, step: &Step) -> WorkResult<StepOutput> {
        let _ = step;
        Ok(StepOutput {
            success: true,
            summary: format!("Completed: {}", step.description),
            artifacts: Vec::new(),
            duration: std::time::Duration::from_millis(50),
        })
    }

    // ------------------------------------------------------------------
    // §3.3 — LLM decomposition
    // ------------------------------------------------------------------

    /// Decompose a work item into execution steps using LLM reasoning.
    ///
    /// Called when `auto_decompose` is enabled and the item has no
    /// predefined steps. Real implementation should call the configured
    /// LLM; current placeholder returns a generic plan.
    async fn decompose_with_llm(&self, item: &WorkItem) -> Vec<Step> {
        let max_retries = self.config.retry.max_step_retries;
        let mut steps = Vec::new();

        steps.push(Step {
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
            steps.push(Step {
                index: steps.len(),
                description: format!("Implement: {}", item.title),
                tool: Some("file".into()),
                expect_llm: true,
                max_retries,
            });
        }
        if desc_lower.contains("test")
            || desc_lower.contains("review")
            || desc_lower.contains("verify")
        {
            steps.push(Step {
                index: steps.len(),
                description: format!("Verify: {}", item.title),
                tool: Some("exec".into()),
                expect_llm: false,
                max_retries: 1,
            });
        }

        steps.push(Step {
            index: steps.len(),
            description: format!("Finalize: {}", item.title),
            tool: None,
            expect_llm: true,
            max_retries: 1,
        });

        steps
    }

    // ------------------------------------------------------------------
    // Retry guard
    // ------------------------------------------------------------------

    /// Decide whether a failed work item should be retried.
    ///
    /// The doc reserves this as a configurable policy point. Currently
    /// delegates to the error's own `retryable` flag (already checked by
    /// the caller), but can be extended with backoff / max-retry tracking.
    fn should_retry(&self, error: &WorkError) -> bool {
        // Always retry if the error is marked retryable.
        // Future: check retry count, backoff, error code denylist.
        let _ = error;
        true
    }

    // ------------------------------------------------------------------
    // Event publishing
    // ------------------------------------------------------------------

    /// Publish a [`WorkEvent`] to the local bus.
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

    /// Publish an internal [`StepEvent`] to the local bus.
    ///
    /// This keeps the bus non-empty during multi-step execution (§4.3).
    async fn publish_step_event(&self, step_index: usize) -> WorkResult<()> {
        let payload = serde_json::to_value(StepEvent { step_index }).map_err(|e| WorkError {
            code: "serialization_error".into(),
            message: e.to_string(),
            retryable: false,
        })?;
        let ev = Event::new(WORK_SOURCE, EventType::Custom(WORK_STEP_KIND.into()), payload);
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
        let old_state = ctx.state;
        ctx.state = new_state;
        debug!(
            agent_id = %self.agent_id,
            from = ?old_state,
            to = ?new_state,
            "WorkSystem state transition",
        );
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
    // Idle signal injection (§7.3)
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

/// Build a [`WorkItemResult`] from the current context.
fn collect_result(ctx: &WorkContext) -> WorkItemResult {
    let item_id = ctx
        .current
        .as_ref()
        .map(|i| i.id)
        .unwrap_or_else(WorkItemId::new);

    let steps_completed = ctx
        .step_outputs
        .iter()
        .filter(|o| o.success)
        .count();
    let steps_failed = ctx
        .step_outputs
        .iter()
        .filter(|o| !o.success)
        .count();
    let total_duration: std::time::Duration = ctx.step_outputs.iter().map(|o| o.duration).sum();

    WorkItemResult {
        item_id,
        outcome: if steps_failed == 0 {
            WorkOutcome::Completed
        } else {
            WorkOutcome::Completed // partial success still completes
        },
        summary: format!(
            "Completed {} steps ({} ok, {} failed)",
            ctx.steps.len(),
            steps_completed,
            steps_failed,
        ),
        steps_completed,
        steps_failed,
        total_duration,
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

    // ── Interrupt (§8) ──────────────────────────────────────────────

    #[tokio::test]
    async fn interrupt_from_busy_goes_to_idle() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
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
        {
            let mut ctx = sys.ctx.lock().await;
            ctx.state = WorkState::Busy;
            ctx.current = Some(item);
            ctx.step_index = 3;
        }

        sys.handle(WorkEvent::Interrupt {
            reason: "user_query".into(),
            by_system: "chat".into(),
        })
        .await
        .expect("handle should succeed");

        let snap = sys.snapshot().await;
        assert_eq!(snap.state, WorkState::Idle);
        assert!(snap.current.is_none());
        // item_id and step_index were captured by the trace event
    }

    // ── WorkItemAssigned (§4.1) ─────────────────────────────────────

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

        // Should be BUSY, queue empty (item dequeued), steps loaded,
        // first StepEvent published.
        let snap = sys.snapshot().await;
        assert_eq!(snap.state, WorkState::Busy);
        assert!(snap.queue.is_empty());
        assert!(snap.current.is_some());
        assert_eq!(snap.steps.len(), 1);
        assert_eq!(snap.step_index, 0);
    }

    #[tokio::test]
    async fn work_item_assigned_when_busy_just_enqueues() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
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
    async fn work_item_assigned_rejects_when_queue_full() {
        let mut config = make_config();
        config.queue.max_size = 1;

        let sys = WorkSystem::new("agent-1", config, make_bus(), make_bus(), None);
        // Pre-fill queue
        {
            let mut ctx = sys.ctx.lock().await;
            ctx.state = WorkState::Busy;
            ctx.current = Some(make_item("running"));
            ctx.enqueue(make_item("queued")); // queue at capacity
        }

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

    // ── Step execution chain (§4.2) ─────────────────────────────────

    #[tokio::test]
    async fn execute_step_advances_to_next_step() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);

        // Set up context with a 2-step work item
        {
            let mut ctx = sys.ctx.lock().await;
            ctx.state = WorkState::Busy;
            ctx.current = Some(make_item("multi-step"));
            ctx.steps = vec![
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
            ];
            ctx.step_index = 0;
        }

        sys.handle_step(0).await.expect("handle_step should succeed");

        // Step 0 done, step_index advanced, output recorded.
        let snap = sys.snapshot().await;
        assert_eq!(snap.step_index, 1);
        assert_eq!(snap.step_outputs.len(), 1);
        assert!(snap.step_outputs[0].success);
    }

    #[tokio::test]
    async fn execute_last_step_finishes_item() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);

        let item_id = WorkItemId::new();
        {
            let mut ctx = sys.ctx.lock().await;
            ctx.state = WorkState::Busy;
            ctx.current = Some(WorkItem {
                id: item_id,
                steps: None,
                ..make_item("single-step")
            });
            ctx.steps = vec![Step {
                index: 0,
                description: "only step".into(),
                tool: None,
                expect_llm: false,
                max_retries: 0,
            }];
            ctx.step_index = 0;
        }

        sys.handle_step(0).await.expect("handle_step should succeed");

        // The last step records the output and step has been cleared from
        // the context (finish_item was called and posted WorkItemCompleted).
        let snap = sys.snapshot().await;
        assert_eq!(snap.step_outputs.len(), 1);
        assert!(snap.step_outputs[0].success);

        // Now simulate the event loop routing WorkItemCompleted back.
        // handle() with WorkItemCompleted triggers process_next → IDLE.
        sys.handle(WorkEvent::WorkItemCompleted {
            item_id,
            result: WorkItemResult {
                item_id,
                outcome: WorkOutcome::Completed,
                summary: "done".into(),
                steps_completed: 1,
                steps_failed: 0,
                total_duration: snap.step_outputs[0].duration,
            },
            duration: snap.step_outputs[0].duration,
        })
        .await
        .expect("handle completion should succeed");

        assert_eq!(sys.current_state().await, WorkState::Idle);
    }

    // ── WorkItemCompleted (§4.1) ────────────────────────────────────

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
        let local_bus = make_bus();
        let sys = WorkSystem::new(
            "agent-1",
            make_config(),
            local_bus.clone(),
            make_bus(),
            None,
        );

        // Pre-populate a queued item.
        {
            let mut ctx = sys.ctx.lock().await;
            ctx.state = WorkState::Busy;
            ctx.current = Some(make_item("finishing"));
            ctx.enqueue(make_item_with_steps(
                "next-up",
                vec![Step {
                    index: 0,
                    description: "do".into(),
                    tool: None,
                    expect_llm: false,
                    max_retries: 0,
                }],
            ));
        }

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

        // Should still be BUSY (next item started), queue empty.
        let snap = sys.snapshot().await;
        assert_eq!(snap.state, WorkState::Busy);
        assert!(snap.queue.is_empty());
        assert!(snap.current.is_some());
        assert_eq!(snap.current.unwrap().title, "next-up");
    }

    // ── WorkItemFailed (§4.1) ───────────────────────────────────────

    #[tokio::test]
    async fn failed_retryable_re_enqueues_to_front() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        let item_id = WorkItemId::new();
        let retry_item = make_item("retry-me");

        {
            let mut ctx = sys.ctx.lock().await;
            ctx.state = WorkState::Busy;
            ctx.current = Some(retry_item);
            // Also have another item in queue to verify push_front.
            ctx.enqueue(make_item("already-queued"));
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
        // The retry-me item was pushed_front, then immediately dequeued by process_next.
        // The already-queued item should still be in queue.
        // But actually process_next starts the next item, so the front item starts.
        // Since retry-me was pushed to front, it should have been started.
        assert!(snap.current.is_some());
    }

    #[tokio::test]
    async fn failed_non_retryable_goes_to_next_or_idle() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        let item_id = WorkItemId::new();

        {
            let mut ctx = sys.ctx.lock().await;
            ctx.state = WorkState::Busy;
            ctx.current = Some(make_item("doomed"));
            // No queued items — should go IDLE.
        }

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

    // ── Full pipeline: assign → step → step → complete → IDLE ──────

    #[tokio::test]
    async fn full_event_driven_pipeline() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);

        // 1. Assign work item with 2 steps.
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

        // System is BUSY, first StepEvent is on the bus.
        assert_eq!(sys.current_state().await, WorkState::Busy);
        let snap = sys.snapshot().await;
        assert_eq!(snap.step_index, 0);
        assert_eq!(snap.steps.len(), 2);

        // 2. Event loop dispatches StepEvent(0) → handle_step(0).
        sys.handle_step(0).await.expect("step 0 should succeed");

        let snap = sys.snapshot().await;
        assert_eq!(snap.step_index, 1);
        assert_eq!(snap.step_outputs.len(), 1);

        // 3. Event loop dispatches StepEvent(1) → handle_step(1).
        //    Last step posts WorkItemCompleted which the event loop
        //    routes back to handle().
        sys.handle_step(1).await.expect("step 1 should succeed");

        let snap = sys.snapshot().await;
        assert_eq!(snap.step_outputs.len(), 2);

        // 4. Simulate event loop routing WorkItemCompleted → handle()
        //    which calls process_next → queue empty → IDLE.
        let item_id = snap.current.as_ref().unwrap().id;
        let total_duration: std::time::Duration =
            snap.step_outputs.iter().map(|o| o.duration).sum();
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

    // ── Convenience methods ─────────────────────────────────────────

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
        assert_eq!(snap.state, WorkState::Idle);
        assert!(snap.queue.is_empty());
    }

    #[tokio::test]
    async fn push_work_item_then_process_to_completion() {
        let sys = WorkSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);

        // Use handle directly (in production, the event loop dispatches).
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

        // System should be BUSY with steps loaded.
        assert_eq!(sys.current_state().await, WorkState::Busy);

        // Execute the single step — posts WorkItemCompleted to bus.
        sys.handle_step(0).await.expect("step should succeed");

        // Simulate event loop routing WorkItemCompleted → handle().
        let snap = sys.snapshot().await;
        let item_id = snap.current.as_ref().unwrap().id;
        let total_duration: std::time::Duration =
            snap.step_outputs.iter().map(|o| o.duration).sum();
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
