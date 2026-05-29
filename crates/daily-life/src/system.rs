// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! DailyLifeSystem — passive FIFO daily routine queue consumer.
//!
//! Architecture ref: daily-life-design.md v2 §4-5.

use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use event_bus::EventBus;
use kernel::agent::AgentSystemState;
use kernel::event::{Event, EventType};
use lifecycle::{LifecycleEngine, LifecycleError};

use crate::config::DailyLifeConfig;
use crate::spec::DailySpec;
use crate::trace::DailyTraceEvent;
use crate::types::{
    DailyContext, DailyError, DailyEvent, DailyItem, DailyItemId, DailyItemSource,
    DailyResult, DailyState, IdleSignal, Priority, Routine, RoutineAction, RoutinePriority,
    TimeWindow, DAILY_SOURCE,
};
use kernel::types::Timestamp;
use std::collections::HashMap;

pub struct DailyLifeSystem {
    engine: LifecycleEngine<DailySpec>,
    config: DailyLifeConfig,
    local_bus: Arc<dyn EventBus>,
    _global_bus: Arc<dyn EventBus>,
    idle_signal_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<IdleSignal>>>,
}

impl DailyLifeSystem {
    #[must_use]
    pub fn new(
        agent_id: impl Into<String>,
        config: DailyLifeConfig,
        local_bus: Arc<dyn EventBus>,
        global_bus: Arc<dyn EventBus>,
        system_state: Option<Arc<std::sync::Mutex<AgentSystemState>>>,
    ) -> Self {
        let spec = DailySpec::new();
        let engine = LifecycleEngine::new(
            agent_id,
            spec,
            config.queue.max_size,
            config.retry.max_step_retries,
            Arc::clone(&local_bus),
            Arc::clone(&global_bus),
            system_state,
            AgentSystemState::DailyLife,
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

    pub async fn current_state(&self) -> DailyState {
        self.engine.current_state().await
    }

    pub async fn snapshot(&self) -> DailyContext {
        let inner = self.engine.snapshot().await;
        DailyContext {
            inner,
            completed_routines: Vec::new(),
        }
    }

    // ------------------------------------------------------------------
    // Main event handler
    // ------------------------------------------------------------------

    pub async fn handle(&self, event: DailyEvent) -> DailyResult<()> {
        debug!(
            "DailyLifeSystem::handle agent_id={} event_kind={}",
            self.engine.agent_id(),
            event.kind(),
        );

        match event {
            DailyEvent::Interrupt { reason, by_system } => {
                self.engine.handle_interrupt(&reason, &by_system).await?;
                self.record_trace(DailyTraceEvent::Interrupted {
                    reason,
                    by_system,
                })
                .await;
                Ok(())
            }

            DailyEvent::DailyItemAssigned { item, source } => {
                let item_id = item.id;
                let source_str = source_name(&source);

                info!(
                    "DailyItemAssigned — enqueuing agent_id={} item_id={} window={:?} source={}",
                    self.engine.agent_id(),
                    item_id,
                    item.window,
                    source_str,
                );

                self.record_trace(DailyTraceEvent::ItemReceived {
                    item_id,
                    window: item.window,
                    source: source_str,
                })
                .await;

                {
                    let snap = self.engine.snapshot().await;
                    if snap.queue_len() >= self.config.queue.max_size {
                        return Err(DailyError {
                            code: "queue_full".into(),
                            message: format!(
                                "Daily queue at capacity ({})",
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

            DailyEvent::DailyItemCompleted {
                item_id,
                outcome,
                duration,
            } => {
                info!(
                    agent_id = %self.engine.agent_id(),
                    %item_id,
                    ?outcome,
                    "DailyItemCompleted"
                );

                self.record_trace(DailyTraceEvent::ItemCompleted {
                    item_id,
                    duration,
                    routines_completed: 0,
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

            DailyEvent::DailyItemFailed {
                item_id,
                error,
                retryable,
            } => {
                warn!(
                    agent_id = %self.engine.agent_id(),
                    %item_id,
                    %retryable,
                    "DailyItemFailed: {}",
                    error.message,
                );

                self.record_trace(DailyTraceEvent::ItemFailed {
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

    /// Handle an internal routine step event.
    pub async fn handle_routine(&self, routine_index: usize) -> DailyResult<()> {
        debug!(
            agent_id = %self.engine.agent_id(),
            routine_index,
            "DailyLifeSystem::handle_routine"
        );
        self.engine.handle_step(routine_index).await?;
        Ok(())
    }

    /// Handle a boredom action tag. Maps tags to light daily activities.
    pub async fn on_boredom_action(&self, tag: &str) {
        let (routine_name, prompt) = match tag {
            "internet" => ("网上冲浪", "浏览最近的技术资讯和行业动态，简要总结有趣的发现"),
            "entertainment" => ("找点乐子", "找一个小娱乐活动——可以是一段有趣的视频、一首歌、或者一个轻松的小游戏"),
            _ => return,
        };
        info!("random_hit:action: {routine_name}");

        let item = DailyItem {
            id: DailyItemId::new(),
            window: TimeWindow::Midday, // any window, routine is explicit
            routines: Some(vec![Routine {
                name: routine_name.into(),
                action: RoutineAction::CustomPrompt {
                    prompt: prompt.into(),
                },
                priority: RoutinePriority::Optional,
            }]),
            priority: Priority::Low,
            context: HashMap::new(),
            notify_on_complete: false,
            created_at: Timestamp::now(),
        };
        let _ = self
            .push_daily_item(
                item,
                DailyItemSource::Custom {
                    name: "boredom".into(),
                    metadata: {
                        let mut m = HashMap::new();
                        m.insert("tag".into(), serde_json::Value::String(tag.into()));
                        m
                    },
                },
            )
            .await;
    }

    pub async fn push_daily_item(
        &self,
        item: DailyItem,
        source: DailyItemSource,
    ) -> DailyResult<()> {
        let event = DailyEvent::DailyItemAssigned { item, source };
        let kind = event.kind().to_string();
        let payload =
            serde_json::to_value(&event).map_err(|e| DailyError {
                code: "serialization_error".into(),
                message: e.to_string(),
                retryable: false,
            })?;
        let ev = Event::new(DAILY_SOURCE, EventType::Custom(kind), payload);
        self.local_bus.publish(ev).await.map_err(|e| DailyError {
            code: "publish_error".into(),
            message: e.to_string(),
            retryable: true,
        })?;
        Ok(())
    }

    pub async fn shutdown(&self) {
        info!(
            "DailyLifeSystem shutting down agent_id={}",
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

    async fn record_trace(&self, event: DailyTraceEvent) {
        debug!(agent_id = %self.engine.agent_id(), trace = ?event, "DailyTraceEvent");
    }

    // ------------------------------------------------------------------
    // Test helpers
    // ------------------------------------------------------------------

    #[doc(hidden)]
    pub async fn _set_state(&self, state: DailyState) {
        self.engine._set_state(state).await;
    }

    #[doc(hidden)]
    pub async fn _set_current(&self, item: DailyItem) {
        self.engine._set_current(item).await;
    }

    #[doc(hidden)]
    pub async fn _set_routines(&self, routines: Vec<crate::types::Routine>) {
        self.engine._set_steps(routines).await;
    }

    #[doc(hidden)]
    pub async fn _set_routine_index(&self, idx: usize) {
        self.engine._set_step_index(idx).await;
    }

    #[doc(hidden)]
    pub async fn _enqueue(&self, item: DailyItem) {
        self.engine._enqueue(item).await;
    }
}

fn source_name(source: &DailyItemSource) -> String {
    match source {
        DailyItemSource::TimeTrigger { trigger, .. } => format!("time_trigger:{trigger}"),
        DailyItemSource::UserAction { .. } => "user_action".into(),
        DailyItemSource::HealthDataSync { .. } => "health_data_sync".into(),
        DailyItemSource::CalendarUpdated => "calendar_updated".into(),
        DailyItemSource::SeekResponse { .. } => "seek_response".into(),
        DailyItemSource::Custom { name, .. } => name.clone(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DailyLifeConfig;
    use crate::types::{DailyItemId, Priority, TimeWindow};
    use event_bus::{EventBus, InMemoryBus, InMemoryBusConfig};
    use kernel::types::Timestamp;
    use lifecycle::LifecycleState;
    use std::collections::HashMap;

    fn make_bus() -> Arc<dyn EventBus> {
        Arc::new(InMemoryBus::new(InMemoryBusConfig::default()))
    }

    fn make_config() -> DailyLifeConfig {
        DailyLifeConfig::default()
    }

    fn make_item(window: TimeWindow) -> DailyItem {
        DailyItem {
            id: DailyItemId::new(),
            window,
            routines: None,
            priority: Priority::default(),
            context: HashMap::new(),
            notify_on_complete: false,
            created_at: Timestamp::now(),
        }
    }

    #[tokio::test]
    async fn system_starts_at_idle() {
        let sys = DailyLifeSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        assert_eq!(sys.current_state().await, DailyState::Idle);
    }

    #[tokio::test]
    async fn interrupt_goes_to_idle() {
        let sys = DailyLifeSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        sys._set_state(LifecycleState::Busy).await;

        sys.handle(DailyEvent::Interrupt {
            reason: "test".into(),
            by_system: "test".into(),
        })
        .await
        .expect("handle should succeed");
        assert_eq!(sys.current_state().await, DailyState::Idle);
    }

    #[tokio::test]
    async fn assigned_morning_enqueues_and_starts() {
        let sys = DailyLifeSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);

        let item = make_item(TimeWindow::Morning);
        sys.handle(DailyEvent::DailyItemAssigned {
            item,
            source: DailyItemSource::TimeTrigger {
                window: TimeWindow::Morning,
                trigger: "morning_tick".into(),
            },
        })
        .await
        .expect("assign should succeed");

        let snap = sys.snapshot().await;
        assert!(!snap.is_idle());
        assert!(snap.current().is_some());
        // Morning should have 4 routines
        assert_eq!(snap.routines().len(), 4);
    }

    #[tokio::test]
    async fn assigned_when_busy_just_enqueues() {
        let sys = DailyLifeSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        sys._set_state(LifecycleState::Busy).await;
        sys._set_current(make_item(TimeWindow::Evening)).await;

        let item = make_item(TimeWindow::Night);
        sys.handle(DailyEvent::DailyItemAssigned {
            item,
            source: DailyItemSource::TimeTrigger {
                window: TimeWindow::Night,
                trigger: "night_tick".into(),
            },
        })
        .await
        .expect("assign should succeed");

        let snap = sys.snapshot().await;
        assert_eq!(snap.queue_len(), 1);
    }

    #[tokio::test]
    async fn execute_routine_advances() {
        let sys = DailyLifeSystem::new("agent-1", make_config(), make_bus(), make_bus(), None);
        use crate::types::{Routine, RoutineAction, RoutinePriority};

        sys._set_state(LifecycleState::Busy).await;
        sys._set_current(make_item(TimeWindow::Midday)).await;
        sys._set_routines(vec![
            Routine {
                name: "r1".into(),
                action: RoutineAction::CheckHabits,
                priority: RoutinePriority::Essential,
            },
            Routine {
                name: "r2".into(),
                action: RoutineAction::CheckHealth,
                priority: RoutinePriority::Optional,
            },
        ])
        .await;

        sys.handle_routine(0).await.expect("routine 0 should succeed");

        let snap = sys.snapshot().await;
        assert_eq!(snap.routine_index(), 1);
        assert_eq!(snap.step_outputs().len(), 1);
    }

    #[tokio::test]
    async fn push_daily_item_publishes_event() {
        let local_bus = make_bus();
        let sys = DailyLifeSystem::new(
            "agent-1",
            make_config(),
            local_bus.clone(),
            make_bus(),
            None,
        );
        let item = make_item(TimeWindow::Night);

        let result = sys
            .push_daily_item(
                item,
                DailyItemSource::TimeTrigger {
                    window: TimeWindow::Night,
                    trigger: "night_tick".into(),
                },
            )
            .await;
        assert!(result.is_ok());
    }
}
