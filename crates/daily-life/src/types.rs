// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Core daily-life system types.
//!
//! Architecture ref: daily-life-design.md v2 §2-3

use kernel::types::Timestamp;
pub use lifecycle::Priority;
use lifecycle::LifecycleState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

pub use lifecycle::{IdleSignal, ItemId, StepOutput};

// ---------------------------------------------------------------------------
// DailyState
// ---------------------------------------------------------------------------

pub type DailyState = LifecycleState;

// ---------------------------------------------------------------------------
// DailyItemId
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DailyItemId(pub Uuid);

impl DailyItemId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for DailyItemId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for DailyItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// TimeWindow
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeWindow {
    Morning,
    Midday,
    Afternoon,
    Evening,
    Night,
}

// ---------------------------------------------------------------------------
// Routine — the internal step type
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Routine {
    pub name: String,
    pub action: RoutineAction,
    pub priority: RoutinePriority,
}

impl Routine {
    #[must_use]
    pub fn max_retries(&self) -> u32 {
        match self.priority {
            RoutinePriority::Essential => 3,
            RoutinePriority::Standard => 2,
            RoutinePriority::Optional => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutineAction {
    CheckCalendar { days_ahead: u32 },
    CheckWeather,
    CheckHabits,
    CheckHealth,
    GuideReflection { template: String },
    DailyBrief,
    CustomPrompt { prompt: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutinePriority {
    Essential,
    Standard,
    Optional,
}

// ---------------------------------------------------------------------------
// DailyItem
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyItem {
    pub id: DailyItemId,
    pub window: TimeWindow,

    /// 预定义的例行事项（可选）。
    pub routines: Option<Vec<Routine>>,

    #[serde(default)]
    pub priority: Priority,

    #[serde(default)]
    pub context: HashMap<String, serde_json::Value>,

    #[serde(default)]
    pub notify_on_complete: bool,

    #[serde(default = "Timestamp::now")]
    pub created_at: Timestamp,
}

// ---------------------------------------------------------------------------
// DailyItemSource
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DailyItemSource {
    TimeTrigger {
        window: TimeWindow,
        trigger: String,
    },
    UserAction {
        operator: String,
        action: String,
    },
    HealthDataSync {
        source: String,
    },
    CalendarUpdated,
    SeekResponse {
        request_id: String,
    },
    Custom {
        name: String,
        #[serde(default)]
        metadata: HashMap<String, serde_json::Value>,
    },
}

// ---------------------------------------------------------------------------
// DailyEvent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "daily_event_type", rename_all = "snake_case")]
pub enum DailyEvent {
    DailyItemAssigned {
        item: DailyItem,
        source: DailyItemSource,
    },
    DailyItemCompleted {
        item_id: DailyItemId,
        outcome: DailyItemOutcome,
        duration: Duration,
    },
    DailyItemFailed {
        item_id: DailyItemId,
        error: DailyError,
        retryable: bool,
    },
    Interrupt {
        reason: String,
        by_system: String,
    },
}

// ---------------------------------------------------------------------------
// DailyItemOutcome / DailyError
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DailyItemOutcome {
    Completed,
    NoRoutines,
    Failed { retryable: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

pub type DailyResult<T> = Result<T, DailyError>;

impl From<lifecycle::LifecycleError> for DailyError {
    fn from(e: lifecycle::LifecycleError) -> Self {
        Self {
            code: e.code,
            message: e.message,
            retryable: e.retryable,
        }
    }
}

// ---------------------------------------------------------------------------
// DailyContext
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DailyContext {
    pub(crate) inner: lifecycle::LifecycleContext<DailyItem, Routine>,
    pub completed_routines: Vec<String>,
}

impl DailyContext {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: lifecycle::LifecycleContext::new(),
            completed_routines: Vec::new(),
        }
    }

    #[must_use]
    pub fn state(&self) -> DailyState {
        self.inner.state
    }

    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.inner.is_idle()
    }

    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.inner.queue_len()
    }

    pub fn enqueue(&mut self, item: DailyItem) {
        self.inner.enqueue(item);
    }

    pub fn dequeue(&mut self) -> Option<DailyItem> {
        self.inner.dequeue()
    }

    #[must_use]
    pub fn current(&self) -> Option<&DailyItem> {
        self.inner.current.as_ref()
    }

    #[must_use]
    pub fn routines(&self) -> &[Routine] {
        &self.inner.steps
    }

    #[must_use]
    pub fn routine_index(&self) -> usize {
        self.inner.step_index
    }

    #[must_use]
    pub fn step_outputs(&self) -> &[StepOutput] {
        &self.inner.step_outputs
    }

    pub fn reset_to_idle(&mut self) {
        self.inner.reset_to_idle();
        self.completed_routines.clear();
    }
}

impl Default for DailyContext {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Event source constants
// ---------------------------------------------------------------------------

pub const DAILY_SOURCE: &str = "daily.system";
pub const DAILY_STEP_KIND: &str = "daily.routine.execute";

impl DailyEvent {
    #[must_use]
    pub fn source(&self) -> &'static str {
        DAILY_SOURCE
    }

    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::DailyItemAssigned { .. } => "daily.item.assigned",
            Self::DailyItemCompleted { .. } => "daily.item.completed",
            Self::DailyItemFailed { .. } => "daily.item.failed",
            Self::Interrupt { .. } => "daily.interrupt",
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_state_is_lifecycle_state() {
        let ds: DailyState = LifecycleState::Idle;
        assert_eq!(ds, DailyState::Idle);
    }

    #[test]
    fn daily_context_new_is_idle() {
        let ctx = DailyContext::new();
        assert!(ctx.is_idle());
        assert!(ctx.current().is_none());
        assert_eq!(ctx.queue_len(), 0);
        assert!(ctx.routines().is_empty());
    }

    #[test]
    fn daily_context_enqueue_dequeue() {
        let mut ctx = DailyContext::new();
        let item_a = DailyItem {
            id: DailyItemId::new(),
            window: TimeWindow::Morning,
            routines: None,
            priority: Priority::default(),
            context: HashMap::new(),
            notify_on_complete: false,
            created_at: Timestamp::now(),
        };
        ctx.enqueue(item_a);
        assert_eq!(ctx.queue_len(), 1);
        assert!(ctx.dequeue().is_some());
        assert_eq!(ctx.queue_len(), 0);
    }

    #[test]
    fn daily_event_serde_tagged() {
        let event = DailyEvent::DailyItemAssigned {
            item: DailyItem {
                id: DailyItemId::new(),
                window: TimeWindow::Morning,
                routines: None,
                priority: Priority::default(),
                context: HashMap::new(),
                notify_on_complete: false,
                created_at: Timestamp::now(),
            },
            source: DailyItemSource::TimeTrigger {
                window: TimeWindow::Morning,
                trigger: "morning_tick".into(),
            },
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("daily_item_assigned"), "{json}");
    }

    #[test]
    fn routine_max_retries() {
        assert_eq!(
            Routine {
                name: "test".into(),
                action: RoutineAction::CheckWeather,
                priority: RoutinePriority::Essential,
            }
            .max_retries(),
            3
        );
        assert_eq!(
            Routine {
                name: "test".into(),
                action: RoutineAction::CheckWeather,
                priority: RoutinePriority::Optional,
            }
            .max_retries(),
            1
        );
    }
}
