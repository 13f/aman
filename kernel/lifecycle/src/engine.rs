// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! LifecycleEngine — generic passive FIFO queue consumer.
//!
//! Encapsulates the shared pattern used by work, study, and daily-life systems:
//! - 2-state machine (Idle / Busy)
//! - Event-driven step chaining via internal bus events
//! - Interrupt → checkpoint → IDLE
//! - Idle signal feedback
//! - Global bus notifications
//!
//! Each domain system implements [`SystemSpec`](super::spec::SystemSpec) and
//! wraps a `LifecycleEngine<S>` where `S` is its own spec type.

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use event_bus::EventBus;
use kernel::agent::AgentSystemState;
use kernel::event::{Event, EventType};

use crate::spec::SystemSpec;
use crate::types::{
    IdleSignal, LifecycleContext, LifecycleError, LifecycleResult, LifecycleState, StepOutput,
};

// ---------------------------------------------------------------------------
// LifecycleEngine
// ---------------------------------------------------------------------------

/// Generic lifecycle engine shared by work, study, and daily-life systems.
///
/// Type parameter `S` is the system-specific spec that provides item/step
/// types, event serialization, and step execution logic.
pub struct LifecycleEngine<S: SystemSpec> {
    agent_id: String,
    spec: S,
    ctx: Mutex<LifecycleContext<S::Item, S::Step>>,
    local_bus: Arc<dyn EventBus>,
    global_bus: Arc<dyn EventBus>,
    idle_signal_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<IdleSignal>>>,
    system_state: Option<Arc<std::sync::Mutex<AgentSystemState>>>,
    /// Which AgentSystemState variant to set when Busy.
    busy_system_state: AgentSystemState,
    /// Max queue size.
    max_queue_size: usize,
    /// Max step retries.
    max_step_retries: u32,
}

impl<S: SystemSpec> LifecycleEngine<S> {
    /// Create a new lifecycle engine.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent_id: impl Into<String>,
        spec: S,
        max_queue_size: usize,
        max_step_retries: u32,
        local_bus: Arc<dyn EventBus>,
        global_bus: Arc<dyn EventBus>,
        system_state: Option<Arc<std::sync::Mutex<AgentSystemState>>>,
        busy_system_state: AgentSystemState,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            spec,
            ctx: Mutex::new(LifecycleContext::new()),
            local_bus,
            global_bus,
            idle_signal_tx: Mutex::new(None),
            system_state,
            busy_system_state,
            max_queue_size,
            max_step_retries,
        }
    }

    /// Set the idle signal channel.
    pub async fn set_idle_signal_tx(
        &self,
        tx: tokio::sync::mpsc::UnboundedSender<IdleSignal>,
    ) {
        *self.idle_signal_tx.lock().await = Some(tx);
    }

    #[must_use]
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Returns the current lifecycle state.
    pub async fn current_state(&self) -> LifecycleState {
        self.ctx.lock().await.state
    }

    /// Returns a snapshot of the context.
    pub async fn snapshot(&self) -> LifecycleContext<S::Item, S::Step> {
        self.ctx.lock().await.clone()
    }

    // ------------------------------------------------------------------
    // Main event handler
    // ------------------------------------------------------------------

    /// Handle an external event. Called by the agent event loop when a
    /// routed event arrives on the local bus.
    ///
    /// The caller deserializes the raw bus event into the system-specific
    /// event enum, then calls the appropriate dispatch method here.
    pub async fn handle_assigned(
        &self,
        item: S::Item,
        _source: serde_json::Value,
    ) -> LifecycleResult<()> {
        let item_id = S::item_id(&item);
        info!(
            agent_id = %self.agent_id,
            %item_id,
            "LifecycleEngine: item assigned — enqueuing",
        );

        let should_start;
        {
            let mut ctx = self.ctx.lock().await;
            if ctx.queue.len() >= self.max_queue_size {
                return Err(LifecycleError {
                    code: "queue_full".into(),
                    message: format!("Queue at capacity ({})", self.max_queue_size),
                    retryable: false,
                });
            }
            ctx.enqueue(item);
            should_start = ctx.is_idle();
        }

        if should_start {
            self.transition_to(LifecycleState::Busy).await;
            self.start_item().await?;
        }
        Ok(())
    }

    pub async fn handle_completed(
        &self,
        item_id: &str,
        result: serde_json::Value,
        _duration_secs: f64,
    ) -> LifecycleResult<()> {
        info!(
            agent_id = %self.agent_id,
            %item_id,
            "LifecycleEngine: item completed",
        );

        // Notify global bus if the item requested it.
        {
            let ctx = self.ctx.lock().await;
            if let Some(ref item) = ctx.current
                && S::notify_on_complete(item)
            {
                let _ = self
                    .global_bus
                    .publish(Event::new(
                        S::event_source(),
                        EventType::Custom(format!("{}.result", S::event_source())),
                        S::make_result_notify(item_id, &result, &self.agent_id),
                    ))
                    .await;
            }
        }

        let item_id_typed = crate::types::ItemId::new();
        self.send_idle_signal(IdleSignal::Satisfaction {
            item_id: item_id_typed,
        })
        .await;

        self.process_next().await
    }

    pub async fn handle_failed(
        &self,
        item_id: &str,
        error: LifecycleError,
        retryable: bool,
    ) -> LifecycleResult<()> {
        warn!(
            agent_id = %self.agent_id,
            %item_id,
            %retryable,
            "LifecycleEngine: item failed: {}",
            error.message,
        );

        if retryable && self.should_retry(&error) {
            if let Some(current) = {
                let mut ctx = self.ctx.lock().await;
                ctx.current.take()
            } {
                let mut ctx = self.ctx.lock().await;
                ctx.push_front(current);
            }
        } else {
            let _ = self
                .global_bus
                .publish(Event::new(
                    S::event_source(),
                    EventType::Custom(format!("{}.failed_event", S::event_source())),
                    S::make_failure_notify(item_id, &error.message, &self.agent_id),
                ))
                .await;

            self.send_idle_signal(IdleSignal::Frustration {
                reason: Some(error.message),
            })
            .await;
        }

        self.process_next().await
    }

    pub async fn handle_interrupt(&self, reason: &str, by_system: &str) -> LifecycleResult<()> {
        let checkpoint;
        {
            let mut ctx = self.ctx.lock().await;
            checkpoint = ctx.interrupt(reason);
        }
        info!(
            agent_id = %self.agent_id,
            %by_system,
            %reason,
            ?checkpoint,
            "LifecycleEngine: interrupted → IDLE",
        );
        self.transition_to(LifecycleState::Idle).await;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Step execution entry point
    // ------------------------------------------------------------------

    /// Handle an internal step event from the local bus.
    pub async fn handle_step(&self, step_index: usize) -> LifecycleResult<()> {
        debug!(
            agent_id = %self.agent_id,
            step_index,
            "LifecycleEngine::handle_step",
        );
        self.execute_step(step_index).await
    }

    // ------------------------------------------------------------------
    // Shutdown
    // ------------------------------------------------------------------

    pub async fn shutdown(&self) {
        info!("LifecycleEngine shutting down agent_id={}", self.agent_id);
        let _ = self.handle_interrupt("shutdown", "core").await;
    }

    // ------------------------------------------------------------------
    // Internal: start_item
    // ------------------------------------------------------------------

    async fn start_item(&self) -> LifecycleResult<()> {
        let item = {
            let mut ctx = self.ctx.lock().await;
            ctx.dequeue()
        };

        let item = match item {
            Some(i) => i,
            None => {
                self.transition_to(LifecycleState::Idle).await;
                return Ok(());
            }
        };

        let item_id = S::item_id(&item);

        let steps = self.spec.decompose(&item, self.max_step_retries).await;

        // If decompose returns empty (no predefined steps + auto_decompose off),
        // use the default single step.
        let steps = if steps.is_empty() {
            vec![S::default_step(&item, self.max_step_retries)]
        } else {
            steps
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
            agent_id = %self.agent_id,
            %item_id,
            total_steps,
            "LifecycleEngine: starting item",
        );

        self.publish_step_event(0).await
    }

    // ------------------------------------------------------------------
    // Internal: execute_step
    // ------------------------------------------------------------------

    async fn execute_step(&self, step_index: usize) -> LifecycleResult<()> {
        let (step, item_id) = {
            let ctx = self.ctx.lock().await;
            let step = ctx.steps.get(step_index).cloned();
            let item_id = ctx.current.as_ref().map(|i| S::item_id(i));
            (step, item_id)
        };

        let step = match step {
            Some(s) => s,
            None => {
                warn!(
                    agent_id = %self.agent_id,
                    step_index,
                    "Step index out of range — completing item",
                );
                return self.finish_item().await;
            }
        };

        let item_id = match item_id {
            Some(id) => id,
            None => {
                self.transition_to(LifecycleState::Idle).await;
                return Ok(());
            }
        };

        debug!(
            agent_id = %self.agent_id,
            %item_id,
            step_index,
            "Executing step",
        );

        let step_start = Instant::now();
        let item = {
            let ctx = self.ctx.lock().await;
            ctx.current.clone()
        };
        let item = match item {
            Some(i) => i,
            None => {
                self.transition_to(LifecycleState::Idle).await;
                return Ok(());
            }
        };

        let result = self
            .spec
            .execute_step_impl(&item, &step, step_index)
            .await;
        let step_duration = step_start.elapsed();

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

                if has_more {
                    self.publish_step_event(step_index + 1).await?;
                } else {
                    self.finish_item().await?;
                }
            }
            Err(error) => {
                if error.retryable && S::step_max_retries(&step) > 0 {
                    warn!(
                        agent_id = %self.agent_id,
                        %item_id,
                        step_index,
                        "Step failed (retryable): {}",
                        error.message,
                    );
                    self.publish_step_event(step_index).await?;
                } else {
                    let payload = S::make_failed_payload(&item_id, &error, false);
                    self.publish_event(S::failed_kind(), payload).await?;
                }
            }
        }

        Ok(())
    }

    // ------------------------------------------------------------------
    // Internal: finish_item
    // ------------------------------------------------------------------

    async fn finish_item(&self) -> LifecycleResult<()> {
        let (item_id, result, total_duration) = {
            let ctx = self.ctx.lock().await;
            let item = match ctx.current.as_ref() {
                Some(i) => i,
                None => {
                    self.transition_to(LifecycleState::Idle).await;
                    return Ok(());
                }
            };
            let item_id = S::item_id(item);
            let result = S::collect_result(item, &ctx.step_outputs);
            let total_duration: std::time::Duration =
                ctx.step_outputs.iter().map(|o| o.duration).sum();
            (item_id, result, total_duration)
        };

        info!(
            agent_id = %self.agent_id,
            %item_id,
            total_duration_ms = total_duration.as_millis(),
            "LifecycleEngine: item finished — posting completion",
        );

        let payload =
            S::make_completed_payload(&item_id, result, total_duration.as_secs_f64());
        self.publish_event(S::completed_kind(), payload).await
    }

    // ------------------------------------------------------------------
    // Internal: process_next
    // ------------------------------------------------------------------

    async fn process_next(&self) -> LifecycleResult<()> {
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
            self.start_item().await?;
        } else {
            self.transition_to(LifecycleState::Idle).await;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Event publishing helpers
    // ------------------------------------------------------------------

    async fn publish_event(&self, kind: &str, payload: serde_json::Value) -> LifecycleResult<()> {
        let ev = Event::new(
            S::event_source(),
            EventType::Custom(kind.to_string()),
            payload,
        );
        self.local_bus.publish(ev).await.map_err(|e| LifecycleError {
            code: "publish_error".into(),
            message: e.to_string(),
            retryable: true,
        })?;
        Ok(())
    }

    async fn publish_step_event(&self, step_index: usize) -> LifecycleResult<()> {
        let payload = S::make_step_payload(step_index);
        let ev = Event::new(
            S::event_source(),
            EventType::Custom(S::step_event_kind().to_string()),
            payload,
        );
        self.local_bus.publish(ev).await.map_err(|e| LifecycleError {
            code: "publish_error".into(),
            message: e.to_string(),
            retryable: true,
        })?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // State transitions
    // ------------------------------------------------------------------

    async fn transition_to(&self, new_state: LifecycleState) {
        let mut ctx = self.ctx.lock().await;
        let old_state = ctx.state;
        ctx.state = new_state;
        debug!(
            agent_id = %self.agent_id,
            from = ?old_state,
            to = ?new_state,
            "LifecycleEngine: state transition",
        );
        drop(ctx);

        if let Some(ref ss) = self.system_state {
            let val = match new_state {
                LifecycleState::Idle => AgentSystemState::Idle,
                LifecycleState::Busy => self.busy_system_state,
            };
            *ss.lock().expect("system_state lock") = val;
        }
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
    // Retry guard
    // ------------------------------------------------------------------

    fn should_retry(&self, _error: &LifecycleError) -> bool {
        true
    }

    // ------------------------------------------------------------------
    // Internal test helpers — accessible across crates via re-export
    // ------------------------------------------------------------------

    #[doc(hidden)]
    pub async fn _set_state(&self, state: LifecycleState) {
        self.ctx.lock().await.state = state;
    }

    #[doc(hidden)]
    pub async fn _set_current(&self, item: S::Item) {
        self.ctx.lock().await.current = Some(item);
    }

    #[doc(hidden)]
    pub async fn _set_steps(&self, steps: Vec<S::Step>) {
        self.ctx.lock().await.steps = steps;
    }

    #[doc(hidden)]
    pub async fn _set_step_index(&self, idx: usize) {
        self.ctx.lock().await.step_index = idx;
    }

    #[doc(hidden)]
    pub async fn _enqueue(&self, item: S::Item) {
        self.ctx.lock().await.enqueue(item);
    }
}
