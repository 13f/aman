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

// ---------------------------------------------------------------------------
// §2.1 WorkState — 两种工作状态
// ---------------------------------------------------------------------------

/// Work System 的状态枚举（v2 简化：2 状态）。
///
/// **IDLE** 是中断入口——队列为空时 Event Bus 空闲，Idle System 自然运行。
/// 收到 `Interrupt` 事件时，无论当前处于何种状态，都无条件切回 IDLE。
/// **BUSY** 期间通过链式投递 StepEvent 保持 Bus 非空。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    /// 工作闲置。队列为空，Event Bus 空闲。
    Idle,
    /// 正在执行当前 WorkItem 的某个步骤。
    Busy,
}

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
// Priority
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low = 1,
    #[default]
    Normal = 2,
    High = 3,
    Critical = 4,
}

// ---------------------------------------------------------------------------
// §3.3 WorkItem
// ---------------------------------------------------------------------------

/// 推送到 Work 队列的工作单元。
///
/// 比 "Task" 更通用：可以是用户指派的任务、看板卡片、API 触发、
/// 定时提醒、Idle Boredom 找回的活——任何需要 Agent 执行的工作。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: WorkItemId,
    pub title: String,
    pub description: String,

    /// 预设的执行步骤（可选）。
    /// 如果为 None，Work System 调用 LLM 自行分解。
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
    /// 通过 aman CLI 直接指派。
    Cli { operator: String },
    /// 通过 HTTP API 指派。
    Api { endpoint: String, operator: String },
    /// 看板插件调度器分配。
    Kanban { board_id: String, scheduler: String },
    /// Todo 列表插件分配。
    Todo { list_id: String },
    /// Agent 在 Boredom 状态下主动 SeekTask 后，调度器响应。
    SeekResponse { request_id: String },
    /// 其他自定义来源。
    Custom {
        name: String,
        #[serde(default)]
        metadata: HashMap<String, serde_json::Value>,
    },
}

// ---------------------------------------------------------------------------
// Step & StepOutput
// ---------------------------------------------------------------------------

/// A decomposed sub-step of a work item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub index: usize,
    pub description: String,
    /// 指定工具名（可选）。
    #[serde(default)]
    pub tool: Option<String>,
    /// 是否需要 LLM 推理。
    #[serde(default)]
    pub expect_llm: bool,
    /// 最大重试次数。
    #[serde(default)]
    pub max_retries: u32,
}

/// Output from executing a single step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutput {
    pub success: bool,
    pub summary: String,
    #[serde(default)]
    pub artifacts: Vec<String>,
    pub duration: Duration,
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
// §3.1 WorkEvent — 工作事件类型（v2: 3 + Interrupt）
// ---------------------------------------------------------------------------

/// Work System 的领域事件。
///
/// v2 简化：只有 3 个业务事件 + Interrupt。外部系统通过 WorkItemAssigned
/// 推送工作项，步骤执行通过内部的 StepEvent 链式流转。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "work_event_type", rename_all = "snake_case")]
pub enum WorkEvent {
    /// 外部系统推送工作项到 Agent。
    WorkItemAssigned {
        item: WorkItem,
        source: WorkItemSource,
    },

    /// 当前工作项执行完成。
    WorkItemCompleted {
        item_id: WorkItemId,
        result: WorkItemResult,
        duration: Duration,
    },

    /// 当前工作项执行失败。
    WorkItemFailed {
        item_id: WorkItemId,
        error: WorkError,
        /// 是否可重试（true 时 WorkItem 重新入队）。
        retryable: bool,
    },

    /// 中断当前执行，强制切回 IDLE。
    /// 任何状态收到此事件 → 保存 checkpoint → 无条件 IDLE。
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
// IdleSignal — feedback to Idle System
// ---------------------------------------------------------------------------

/// Signals that the Work System sends to the Idle System via coordination.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleSignal {
    /// Work item completed successfully — boosts satisfaction.
    Satisfaction {
        work_item_id: WorkItemId,
    },
    /// Work item failed — adds frustration.
    Frustration {
        reason: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// §3.4 WorkContext — 工作上下文
// ---------------------------------------------------------------------------

/// Work System 的共享状态。全部 Agent 内部私有。
#[derive(Debug, Clone)]
pub struct WorkContext {
    /// 当前工作状态。
    pub state: WorkState,
    /// FIFO 工作队列。
    pub queue: std::collections::VecDeque<WorkItem>,
    /// 当前正在执行的工作项。
    pub current: Option<WorkItem>,
    /// 当前工作项的步骤列表。
    pub steps: Vec<Step>,
    /// 当前步骤索引。
    pub step_index: usize,
    /// Collected step outputs.
    pub step_outputs: Vec<StepOutput>,
}

impl WorkContext {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: WorkState::Idle,
            queue: std::collections::VecDeque::new(),
            current: None,
            steps: Vec::new(),
            step_index: 0,
            step_outputs: Vec::new(),
        }
    }

    /// 推入工作项到队列尾部。
    pub fn enqueue(&mut self, item: WorkItem) {
        self.queue.push_back(item);
    }

    /// 取出下一个待执行工作项。
    pub fn dequeue(&mut self) -> Option<WorkItem> {
        self.queue.pop_front()
    }

    /// 重置为 IDLE，清空当前工作上下文。
    pub fn reset_to_idle(&mut self) {
        self.state = WorkState::Idle;
        self.current = None;
        self.steps.clear();
        self.step_index = 0;
        self.step_outputs.clear();
    }

    /// 中断当前工作项，保存 checkpoint 后回到 IDLE。
    pub fn interrupt(&mut self, reason: &str) -> WorkCheckpoint {
        let checkpoint = WorkCheckpoint {
            state: self.state,
            item_id: self.current.as_ref().map(|w| w.id),
            step_index: self.step_index,
            timestamp: Timestamp::now(),
            reason: reason.to_string(),
        };
        self.reset_to_idle();
        checkpoint
    }
}

impl Default for WorkContext {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// WorkCheckpoint
// ---------------------------------------------------------------------------

/// Saved progress point when interrupted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkCheckpoint {
    pub state: WorkState,
    pub item_id: Option<WorkItemId>,
    pub step_index: usize,
    pub timestamp: Timestamp,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// WorkResult
// ---------------------------------------------------------------------------

/// Alias for work system operation results.
pub type WorkResult<T> = Result<T, WorkError>;

// ---------------------------------------------------------------------------
// Global bus notification events (posted outside the Work System)
// ---------------------------------------------------------------------------

/// Posted to the global bus when a work item completes and
/// `notify_on_complete` is true. External systems (kanban, todo, CLI)
/// subscribe to this to learn about completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItemResultEvent {
    pub item_id: WorkItemId,
    pub result: WorkItemResult,
    pub agent_id: String,
}

/// Posted to the global bus when a work item fails and is not retryable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItemFailedEvent {
    pub item_id: WorkItemId,
    pub error: String,
    pub agent_id: String,
}

// ---------------------------------------------------------------------------
// Event source constants for routing
// ---------------------------------------------------------------------------

/// Event source prefix for work events published to the EventBus.
pub const WORK_SOURCE: &str = "work.system";

/// Event kind for internal step execution events.
pub const WORK_STEP_KIND: &str = "work.step.execute";

impl WorkEvent {
    /// Returns the event source string for routing purposes.
    #[must_use]
    pub fn source(&self) -> &'static str {
        WORK_SOURCE
    }

    /// Returns a discriminant string for event routing.
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
// Internal StepEvent — 步骤执行链（不暴露为 WorkEvent）
// ---------------------------------------------------------------------------

/// 内部步骤事件——Work System 内部链式流转。
/// Published to the local bus to keep it non-empty during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StepEvent {
    pub step_index: usize,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── WorkState ────────────────────────────────────────────────

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
        assert_eq!(ctx.state, WorkState::Idle);
        assert!(ctx.current.is_none());
        assert!(ctx.queue.is_empty());
        assert!(ctx.steps.is_empty());
        assert_eq!(ctx.step_index, 0);
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
        assert_eq!(ctx.queue.len(), 2);

        let first = ctx.dequeue().unwrap();
        assert_eq!(first.title, "A");
        let second = ctx.dequeue().unwrap();
        assert_eq!(second.title, "B");
        assert!(ctx.dequeue().is_none());
    }

    #[test]
    fn context_reset_to_idle_clears_all() {
        let mut ctx = WorkContext {
            state: WorkState::Busy,
            current: Some(WorkItem {
                id: WorkItemId::new(),
                title: "test".into(),
                description: String::new(),
                steps: None,
                priority: Priority::default(),
                timeout: None,
                context: HashMap::new(),
                notify_on_complete: false,
                created_at: Timestamp::now(),
            }),
            steps: vec![Step {
                index: 0,
                description: "do thing".into(),
                tool: None,
                expect_llm: false,
                max_retries: 0,
            }],
            step_index: 3,
            queue: std::collections::VecDeque::new(),
            step_outputs: vec![StepOutput {
                success: true,
                summary: "done".into(),
                artifacts: vec![],
                duration: Duration::from_secs(1),
            }],
        };
        ctx.reset_to_idle();
        assert_eq!(ctx.state, WorkState::Idle);
        assert!(ctx.current.is_none());
        assert!(ctx.steps.is_empty());
        assert_eq!(ctx.step_index, 0);
        assert!(ctx.step_outputs.is_empty());
    }

    #[test]
    fn context_interrupt_saves_checkpoint_and_resets() {
        let mut ctx = WorkContext::new();
        ctx.state = WorkState::Busy;
        ctx.step_index = 5;
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
        ctx.current = Some(item);

        let checkpoint = ctx.interrupt("user_query");

        assert_eq!(checkpoint.item_id, Some(item_id));
        assert_eq!(checkpoint.step_index, 5);
        assert_eq!(checkpoint.state, WorkState::Busy);
        assert_eq!(ctx.state, WorkState::Idle);
        assert!(ctx.current.is_none());
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
            work_item_id: WorkItemId::new(),
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
