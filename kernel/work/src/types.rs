// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Core work system types: WorkState, WorkEvent, WorkItem, WorkContext.
//!
//! Architecture ref: work-design.md v2 §3

use kernel::types::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

// Re-export shared lifecycle types for backward compatibility.
pub use lifecycle::{
    Checkpoint as WorkCheckpoint, IdleSignal, ItemId, LifecycleState, Priority, StepOutput,
};

// ---------------------------------------------------------------------------
// §2.1 WorkState — type alias for LifecycleState
// ---------------------------------------------------------------------------

/// Work system state (v2: 2 states).
pub type WorkState = LifecycleState;

// ---------------------------------------------------------------------------
// WorkItemId
// ---------------------------------------------------------------------------

/// Unique identifier for a work item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkItemId(pub Uuid);

impl WorkItemId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for WorkItemId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for WorkItemId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// §3.3 WorkItem
// ---------------------------------------------------------------------------

/// 推送到 Work 队列的工作单元。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: WorkItemId,
    pub title: String,
    pub description: String,

    /// 预设的执行步骤（可选）。
    pub steps: Option<Vec<Step>>,

    /// 优先级（队列内排序用）。
    #[serde(default)]
    pub priority: Priority,

    /// 执行超时。
    #[serde(default)]
    pub timeout: Option<Duration>,

    /// 附带的上下文数据。
    #[serde(default)]
    pub context: HashMap<String, serde_json::Value>,

    /// 是否需要在完成后通知调用方（通过 Global Bus）。
    #[serde(default)]
    pub notify_on_complete: bool,

    /// 创建时间。
    #[serde(default = "Timestamp::now")]
    pub created_at: Timestamp,
}

// ---------------------------------------------------------------------------
// WorkItemSource
// ---------------------------------------------------------------------------

/// 标识 WorkItem 的来源。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkItemSource {
    Cli { operator: String },
    Api { endpoint: String, operator: String },
    Kanban { board_id: String, scheduler: String },
    Todo { list_id: String },
    SeekResponse { request_id: String },
    Custom {
        name: String,
        #[serde(default)]
        metadata: HashMap<String, serde_json::Value>,
    },
}

// ---------------------------------------------------------------------------
// Step
// ---------------------------------------------------------------------------

/// A decomposed sub-step of a work item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub index: usize,
    pub description: String,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub expect_llm: bool,
    #[serde(default)]
    pub max_retries: u32,
}

// ---------------------------------------------------------------------------
// WorkItemResult
// ---------------------------------------------------------------------------

/// Final result of a completed work item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItemResult {
    pub item_id: WorkItemId,
    pub outcome: WorkOutcome,
    pub summary: String,
    pub steps_completed: usize,
    pub steps_failed: usize,
    pub total_duration: Duration,
}

// ---------------------------------------------------------------------------
// §3.1 WorkEvent
// ---------------------------------------------------------------------------

/// Work System 的领域事件（v2: 3 + Interrupt）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "work_event_type", rename_all = "snake_case")]
pub enum WorkEvent {
    WorkItemAssigned {
        item: WorkItem,
        source: WorkItemSource,
    },
    WorkItemCompleted {
        item_id: WorkItemId,
        result: WorkItemResult,
        duration: Duration,
    },
    WorkItemFailed {
        item_id: WorkItemId,
        error: WorkError,
        retryable: bool,
    },
    Interrupt {
        reason: String,
        by_system: String,
    },
}

// ---------------------------------------------------------------------------
// WorkOutcome / WorkError
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkOutcome {
    Completed,
    Failed { retryable: bool },
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

// ---------------------------------------------------------------------------
// Global bus notification events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItemResultEvent {
    pub item_id: WorkItemId,
    pub result: WorkItemResult,
    pub agent_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItemFailedEvent {
    pub item_id: WorkItemId,
    pub error: String,
    pub agent_id: String,
}

// ---------------------------------------------------------------------------
// Event source constants
// ---------------------------------------------------------------------------

pub const WORK_SOURCE: &str = "work.system";
pub const WORK_STEP_KIND: &str = "work.step.execute";

impl WorkEvent {
    #[must_use]
    pub fn source(&self) -> &'static str {
        WORK_SOURCE
    }

    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::WorkItemAssigned { .. } => "work.item.assigned",
            Self::WorkItemCompleted { .. } => "work.item.completed",
            Self::WorkItemFailed { .. } => "work.item.failed",
            Self::Interrupt { .. } => "work.interrupt",
        }
    }
}

// ---------------------------------------------------------------------------
// WorkContext — delegates to lifecycle::LifecycleContext
// ---------------------------------------------------------------------------

/// Work system shared state. Wraps the generic lifecycle context.
#[derive(Debug, Clone)]
pub struct WorkContext {
    pub(crate) inner: lifecycle::LifecycleContext<WorkItem, Step>,
}

impl WorkContext {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: lifecycle::LifecycleContext::new(),
        }
    }

    #[must_use]
    pub fn state(&self) -> WorkState {
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

    pub fn enqueue(&mut self, item: WorkItem) {
        self.inner.enqueue(item);
    }

    pub fn dequeue(&mut self) -> Option<WorkItem> {
        self.inner.dequeue()
    }

    #[must_use]
    pub fn current(&self) -> Option<&WorkItem> {
        self.inner.current.as_ref()
    }

    #[must_use]
    pub fn steps(&self) -> &[Step] {
        &self.inner.steps
    }

    #[must_use]
    pub fn step_index(&self) -> usize {
        self.inner.step_index
    }

    #[must_use]
    pub fn step_outputs(&self) -> &[StepOutput] {
        &self.inner.step_outputs
    }

    pub fn reset_to_idle(&mut self) {
        self.inner.reset_to_idle();
    }

    pub fn interrupt(&mut self, reason: &str) -> lifecycle::Checkpoint {
        self.inner.interrupt(reason)
    }
}

impl Default for WorkContext {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// WorkResult / From impls
// ---------------------------------------------------------------------------

pub type WorkResult<T> = Result<T, WorkError>;

impl From<lifecycle::LifecycleError> for WorkError {
    fn from(e: lifecycle::LifecycleError) -> Self {
        Self {
            code: e.code,
            message: e.message,
            retryable: e.retryable,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── WorkState ────────────────────────────────────────────────

    #[test]
    fn work_state_is_lifecycle_state() {
        // Verify type alias compiles and values match
        let ws: WorkState = LifecycleState::Idle;
        assert_eq!(ws, WorkState::Idle);
    }

    #[test]
    fn work_state_serde_roundtrip() {
        for state in &[WorkState::Idle, WorkState::Busy] {
            let json = serde_json::to_string(state).expect("serialize");
            let deserialized: WorkState = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*state, deserialized);
        }
    }

    // ── WorkItemId ───────────────────────────────────────────────

    #[test]
    fn work_item_id_default_is_v7_uuid() {
        let id = WorkItemId::default();
        assert_eq!(id.0.get_version_num(), 7);
    }

    #[test]
    fn work_item_id_display() {
        let id = WorkItemId::new();
        assert_eq!(format!("{id}"), id.0.to_string());
    }

    // ── Priority ─────────────────────────────────────────────────

    #[test]
    fn priority_ordering() {
        assert!(Priority::Critical > Priority::High);
        assert!(Priority::High > Priority::Normal);
        assert!(Priority::Normal > Priority::Low);
    }

    #[test]
    fn priority_default_is_normal() {
        assert_eq!(Priority::default(), Priority::Normal);
    }

    // ── WorkContext ──────────────────────────────────────────────

    #[test]
    fn context_new_is_idle() {
        let ctx = WorkContext::new();
        assert!(ctx.is_idle());
        assert!(ctx.current().is_none());
        assert_eq!(ctx.queue_len(), 0);
        assert!(ctx.steps().is_empty());
        assert_eq!(ctx.step_index(), 0);
    }

    #[test]
    fn context_enqueue_dequeue_fifo() {
        let mut ctx = WorkContext::new();
        let item_a = WorkItem {
            id: WorkItemId::new(),
            title: "A".into(),
            description: String::new(),
            steps: None,
            priority: Priority::default(),
            timeout: None,
            context: HashMap::new(),
            notify_on_complete: false,
            created_at: Timestamp::now(),
        };
        let item_b = WorkItem {
            id: WorkItemId::new(),
            title: "B".into(),
            ..item_a.clone()
        };

        ctx.enqueue(item_a.clone());
        ctx.enqueue(item_b.clone());
        assert_eq!(ctx.queue_len(), 2);

        let first = ctx.dequeue().unwrap();
        assert_eq!(first.title, "A");
        let second = ctx.dequeue().unwrap();
        assert_eq!(second.title, "B");
        assert!(ctx.dequeue().is_none());
    }

    #[test]
    fn context_reset_to_idle_clears_all() {
        let mut ctx = WorkContext::new();
        ctx.inner.state = WorkState::Busy;
        ctx.inner.current = Some(WorkItem {
            id: WorkItemId::new(),
            title: "test".into(),
            description: String::new(),
            steps: None,
            priority: Priority::default(),
            timeout: None,
            context: HashMap::new(),
            notify_on_complete: false,
            created_at: Timestamp::now(),
        });
        ctx.inner.steps = vec![Step {
            index: 0,
            description: "do thing".into(),
            tool: None,
            expect_llm: false,
            max_retries: 0,
        }];
        ctx.inner.step_index = 3;
        ctx.inner.step_outputs = vec![StepOutput {
            success: true,
            summary: "done".into(),
            artifacts: vec![],
            duration: Duration::from_secs(1),
        }];
        ctx.reset_to_idle();
        assert!(ctx.is_idle());
        assert!(ctx.current().is_none());
        assert!(ctx.steps().is_empty());
        assert_eq!(ctx.step_index(), 0);
        assert!(ctx.step_outputs().is_empty());
    }

    #[test]
    fn context_interrupt_saves_checkpoint_and_resets() {
        let mut ctx = WorkContext::new();
        ctx.inner.state = WorkState::Busy;
        ctx.inner.step_index = 5;
        let item = WorkItem {
            id: WorkItemId::new(),
            title: "task".into(),
            description: String::new(),
            steps: None,
            priority: Priority::default(),
            timeout: None,
            context: HashMap::new(),
            notify_on_complete: false,
            created_at: Timestamp::now(),
        };
        let item_id = item.id;
        ctx.inner.current = Some(item);

        let checkpoint = ctx.interrupt("user_query");

        assert_eq!(checkpoint.step_index, 5);
        assert!(ctx.is_idle());
        let _ = item_id; // kept for context
    }

    // ── WorkEvent ────────────────────────────────────────────────

    #[test]
    fn work_event_kind_discriminants() {
        assert_eq!(
            WorkEvent::Interrupt {
                reason: "test".into(),
                by_system: "core".into(),
            }
            .kind(),
            "work.interrupt"
        );
        assert_eq!(
            WorkEvent::WorkItemAssigned {
                item: WorkItem {
                    id: WorkItemId::new(),
                    title: "t".into(),
                    description: String::new(),
                    steps: None,
                    priority: Priority::default(),
                    timeout: None,
                    context: HashMap::new(),
                    notify_on_complete: false,
                    created_at: Timestamp::now(),
                },
                source: WorkItemSource::Cli {
                    operator: "user".into(),
                },
            }
            .kind(),
            "work.item.assigned"
        );
    }

    #[test]
    fn work_event_serde_tagged() {
        let event = WorkEvent::WorkItemAssigned {
            item: WorkItem {
                id: WorkItemId::new(),
                title: "test".into(),
                description: "desc".into(),
                steps: None,
                priority: Priority::default(),
                timeout: None,
                context: HashMap::new(),
                notify_on_complete: false,
                created_at: Timestamp::now(),
            },
            source: WorkItemSource::Cli {
                operator: "user".into(),
            },
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(
            json.contains("work_item_assigned"),
            "expected tagged: {json}"
        );

        let deserialized: WorkEvent = serde_json::from_str(&json).expect("deserialize");
        match deserialized {
            WorkEvent::WorkItemAssigned { .. } => {}
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn work_event_interrupt_serde() {
        let event = WorkEvent::Interrupt {
            reason: "shutdown".into(),
            by_system: "core".into(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let deser: WorkEvent = serde_json::from_str(&json).expect("deserialize");
        match deser {
            WorkEvent::Interrupt { reason, .. } => assert_eq!(reason, "shutdown"),
            _ => panic!("wrong variant"),
        }
    }

    // ── IdleSignal ───────────────────────────────────────────────

    #[test]
    fn idle_signal_serde() {
        let sig = IdleSignal::Satisfaction {
            item_id: ItemId::new(),
        };
        let json = serde_json::to_string(&sig).expect("serialize");
        let deser: IdleSignal = serde_json::from_str(&json).expect("deserialize");
        match deser {
            IdleSignal::Satisfaction { .. } => {}
            _ => panic!("wrong variant"),
        }
    }

    // ── WorkItemSource ───────────────────────────────────────────

    #[test]
    fn work_item_source_serde_tagged() {
        let src = WorkItemSource::Kanban {
            board_id: "kb-1".into(),
            scheduler: "auto".into(),
        };
        let json = serde_json::to_string(&src).expect("serialize");
        assert!(json.contains("kanban"), "expected tagged: {json}");
        let deser: WorkItemSource = serde_json::from_str(&json).expect("deserialize");
        match deser {
            WorkItemSource::Kanban { board_id, .. } => assert_eq!(board_id, "kb-1"),
            _ => panic!("wrong variant"),
        }
    }
}
