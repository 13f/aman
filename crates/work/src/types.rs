// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Core work system types: WorkState, WorkEvent, WorkContext, and supporting types.
//!
//! Architecture ref: work-design.md §3

use kernel::types::Timestamp;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// §3.1 WorkState — 五种工作状态
// ---------------------------------------------------------------------------

/// Work System 的状态枚举。
///
/// **IDLE** 是中断入口——也是唯一不占用 Event Bus 的状态（Bus 为空 → 全局 Idle 可运行）。
/// 收到 `Interrupt` 事件时，无论当前处于什么状态，都无条件切回 IDLE。
/// 其他四种状态通过链式投递事件保持 Bus 非空。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    /// 工作闲置。不占用 Event Bus，允许全局 Idle 运行。
    /// 也是中断入口——任何活跃状态收到 Interrupt 后回到此处。
    Idle,
    /// 正在检查任务板（同步操作，极短）。
    Checking,
    /// 正在认领任务（等待 Global Bus 的 ClaimResponse）。
    Claiming,
    /// 正在执行任务的某个子步骤。
    Executing,
    /// 正在复核执行结果。
    Reviewing,
}

// ---------------------------------------------------------------------------
// TaskId & TaskBrief
// ---------------------------------------------------------------------------

/// Unique identifier for a task on a task board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(pub Uuid);

impl TaskId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Brief summary of a task used for selection and claiming.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBrief {
    pub id: TaskId,
    pub title: String,
    pub description: String,
    pub priority: Option<String>,
    pub required_capabilities: Vec<String>,
    pub stage: String,
    pub created_at: Timestamp,
}

// ---------------------------------------------------------------------------
// Step & StepOutput
// ---------------------------------------------------------------------------

/// A decomposed sub-step of a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub index: usize,
    pub description: String,
    pub requires_llm: bool,
    pub requires_tool: bool,
    pub estimated_duration: Duration,
}

/// Output from executing a single step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepOutput {
    pub success: bool,
    pub summary: String,
    pub artifacts: Vec<String>,
    pub duration: Duration,
}

// ---------------------------------------------------------------------------
// TaskResult
// ---------------------------------------------------------------------------

/// Final result submitted back to the task board.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: TaskId,
    pub outcome: WorkOutcome,
    pub summary: String,
    pub steps_completed: usize,
    pub steps_failed: usize,
    pub total_duration: Duration,
}

// ---------------------------------------------------------------------------
// §3.2 WorkEvent — 工作事件类型
// ---------------------------------------------------------------------------

/// Work System 的领域事件。
///
/// 分为三类：
/// - 外部来源：由 Global Event Bus 注入（TaskBoardUpdated）
/// - 定时触发：由 DelayedWorkTick 延迟事件触发
/// - 内部流转：Work System 自身产生，用于状态机流转
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "work_event_type", rename_all = "snake_case")]
pub enum WorkEvent {
    // ── 外部来源（通过 Global Bus → Agent Local Bus）──
    /// kanban/team 插件通知任务板有变动。
    TaskBoardUpdated {
        board_id: String,
        change_type: TaskBoardChangeType,
    },
    /// 外部系统（如 cron、webhook）触发的主动巡检。
    WorkTick {
        triggered_by: String,
    },

    // ── 延迟定时事件 ──
    /// 一段时间后触发 WorkTick，用于冷却期巡检。
    DelayedWorkTick {
        fire_at: Timestamp,
        reason: String,
    },

    // ── 内部状态机流转 ──
    /// 开始检查任务板。
    StartCheck,
    /// 认领指定任务。
    ClaimTask(TaskBrief),
    /// 认领响应。
    ClaimResponse {
        task: TaskBrief,
        success: bool,
        reason: Option<String>,
    },
    /// 执行下一个子步骤。
    ExecuteStep {
        task_id: TaskId,
        step_index: usize,
    },
    /// 子步骤完成。
    StepComplete {
        task_id: TaskId,
        step_index: usize,
        output: StepOutput,
    },
    /// 子步骤失败。
    StepFailed {
        task_id: TaskId,
        step_index: usize,
        error: WorkError,
    },
    /// 开始复核。
    ReviewTask(TaskBrief),
    /// 复核完成。
    ReviewComplete {
        task_id: TaskId,
        passed: bool,
        feedback: Option<String>,
    },
    /// 提交结果到 kanban/team。
    SubmitResult {
        task_id: TaskId,
        result: TaskResult,
    },
    /// 工作周期完成（日志/指标用）。
    WorkCycleDone {
        task_id: TaskId,
        outcome: WorkOutcome,
        duration: Duration,
    },

    // ── 系统中断 ──
    /// 中断当前 Work System，强制切回 IDLE。
    Interrupt {
        reason: String,
        by_system: String,
    },
}

// ---------------------------------------------------------------------------
// Supporting enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskBoardChangeType {
    TaskAdded,
    TaskRemoved,
    TaskUpdated,
    StageBulkMove,
}

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
// Idle signal types (injected into Idle System for feedback loop)
// ---------------------------------------------------------------------------

/// Signals that the Work System sends to the Idle System via coordination.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleSignal {
    /// Task completed successfully — boosts satisfaction.
    Satisfaction {
        task_id: TaskId,
    },
    /// Task failed or couldn't be claimed — adds frustration.
    Frustration {
        reason: Option<String>,
    },
    /// Review found issues — mild disappointment.
    Disappointment {
        task_id: TaskId,
    },
}

// ---------------------------------------------------------------------------
// §3.3 WorkContext — 工作上下文
// ---------------------------------------------------------------------------

/// Work System 的共享状态。
///
/// 全部字段为 Agent 内部私有，不跨 Agent 共享。
#[derive(Debug, Clone)]
pub struct WorkContext {
    /// 当前工作状态。
    pub state: WorkState,
    /// 当前正在处理的任务。
    pub current_task: Option<TaskBrief>,
    /// 任务的子步骤列表（由 decompose_task 产生）。
    pub task_steps: Vec<Step>,
    /// 当前执行到的步骤索引。
    pub step_index: usize,
    /// 上一次检查任务板的时间。
    pub last_check_time: Timestamp,
    /// 连续认领失败的次数（用于退避策略）。
    pub consecutive_claim_failures: u32,
    /// Collected step outputs (for final review/submission).
    pub step_outputs: Vec<StepOutput>,
}

impl WorkContext {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: WorkState::Idle,
            current_task: None,
            task_steps: Vec::new(),
            step_index: 0,
            last_check_time: Timestamp::now(),
            consecutive_claim_failures: 0,
            step_outputs: Vec::new(),
        }
    }

    /// 重置为闲置状态，清空当前任务上下文。
    pub fn reset_to_idle(&mut self) {
        self.state = WorkState::Idle;
        self.current_task = None;
        self.task_steps.clear();
        self.step_index = 0;
        self.step_outputs.clear();
    }

    /// 中断当前任务，保存 checkpoint 后回到 IDLE。
    pub fn interrupt(&mut self, reason: &str) -> WorkCheckpoint {
        let checkpoint = WorkCheckpoint {
            state: self.state,
            task_id: self.current_task.as_ref().map(|t| t.id),
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
    pub task_id: Option<TaskId>,
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
// Event source constants for routing
// ---------------------------------------------------------------------------

/// Event source prefix for work events published to the EventBus.
pub const WORK_SOURCE: &str = "work.system";

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
            Self::TaskBoardUpdated { .. } => "work.task_board_updated",
            Self::WorkTick { .. } => "work.work_tick",
            Self::DelayedWorkTick { .. } => "work.delayed_work_tick",
            Self::StartCheck => "work.start_check",
            Self::ClaimTask(_) => "work.claim_task",
            Self::ClaimResponse { .. } => "work.claim_response",
            Self::ExecuteStep { .. } => "work.execute_step",
            Self::StepComplete { .. } => "work.step_complete",
            Self::StepFailed { .. } => "work.step_failed",
            Self::ReviewTask(_) => "work.review_task",
            Self::ReviewComplete { .. } => "work.review_complete",
            Self::SubmitResult { .. } => "work.submit_result",
            Self::WorkCycleDone { .. } => "work.cycle_done",
            Self::Interrupt { .. } => "work.interrupt",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_state_serde_roundtrip() {
        for state in &[
            WorkState::Idle,
            WorkState::Checking,
            WorkState::Claiming,
            WorkState::Executing,
            WorkState::Reviewing,
        ] {
            let json = serde_json::to_string(state).expect("serialize");
            let deserialized: WorkState = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(*state, deserialized);
        }
    }

    #[test]
    fn task_id_default_is_v7_uuid() {
        let id = TaskId::default();
        assert_eq!(id.0.get_version_num(), 7);
    }

    #[test]
    fn context_new_is_idle() {
        let ctx = WorkContext::new();
        assert_eq!(ctx.state, WorkState::Idle);
        assert!(ctx.current_task.is_none());
        assert!(ctx.task_steps.is_empty());
        assert_eq!(ctx.step_index, 0);
        assert_eq!(ctx.consecutive_claim_failures, 0);
        assert!(ctx.step_outputs.is_empty());
    }

    #[test]
    fn context_reset_to_idle_clears_all() {
        let mut ctx = WorkContext {
            state: WorkState::Executing,
            current_task: Some(TaskBrief {
                id: TaskId::new(),
                title: "test".into(),
                description: "desc".into(),
                priority: None,
                required_capabilities: vec![],
                stage: "backlog".into(),
                created_at: Timestamp::now(),
            }),
            task_steps: vec![Step {
                index: 0,
                description: "do thing".into(),
                requires_llm: false,
                requires_tool: false,
                estimated_duration: Duration::from_secs(10),
            }],
            step_index: 3,
            last_check_time: Timestamp::now(),
            consecutive_claim_failures: 2,
            step_outputs: vec![StepOutput {
                success: true,
                summary: "done".into(),
                artifacts: vec![],
                duration: Duration::from_secs(1),
            }],
        };
        ctx.reset_to_idle();
        assert_eq!(ctx.state, WorkState::Idle);
        assert!(ctx.current_task.is_none());
        assert!(ctx.task_steps.is_empty());
        assert_eq!(ctx.step_index, 0);
        assert!(ctx.step_outputs.is_empty());
    }

    #[test]
    fn context_interrupt_saves_checkpoint_and_resets() {
        let mut ctx = WorkContext::new();
        ctx.state = WorkState::Executing;
        ctx.step_index = 5;
        let task = TaskBrief {
            id: TaskId::new(),
            title: "task".into(),
            description: "desc".into(),
            priority: None,
            required_capabilities: vec![],
            stage: "wip".into(),
            created_at: Timestamp::now(),
        };
        let task_id = task.id;
        ctx.current_task = Some(task);

        let checkpoint = ctx.interrupt("user_query");

        assert_eq!(checkpoint.task_id, Some(task_id));
        assert_eq!(checkpoint.step_index, 5);
        assert_eq!(checkpoint.state, WorkState::Executing);
        assert_eq!(ctx.state, WorkState::Idle);
        assert!(ctx.current_task.is_none());
    }

    #[test]
    fn work_event_kind_discriminants() {
        assert_eq!(
            WorkEvent::StartCheck.kind(),
            "work.start_check"
        );
        assert_eq!(
            WorkEvent::Interrupt {
                reason: "test".into(),
                by_system: "core".into()
            }
            .kind(),
            "work.interrupt"
        );
        assert_eq!(
            WorkEvent::TaskBoardUpdated {
                board_id: "b1".into(),
                change_type: TaskBoardChangeType::TaskAdded,
            }
            .kind(),
            "work.task_board_updated"
        );
    }

    #[test]
    fn work_event_serde_tagged() {
        let event = WorkEvent::ExecuteStep {
            task_id: TaskId::new(),
            step_index: 0,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("execute_step"), "expected tagged: {json}");

        let deserialized: WorkEvent = serde_json::from_str(&json).expect("deserialize");
        match deserialized {
            WorkEvent::ExecuteStep { step_index, .. } => assert_eq!(step_index, 0),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn idle_signal_serde() {
        let sig = IdleSignal::Satisfaction {
            task_id: TaskId::new(),
        };
        let json = serde_json::to_string(&sig).expect("serialize");
        let deser: IdleSignal = serde_json::from_str(&json).expect("deserialize");
        match deser {
            IdleSignal::Satisfaction { .. } => {}
            _ => panic!("wrong variant"),
        }
    }
}
