// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! WorkTraceEvent — events written to the Agent's private Trace Store.
//!
//! Architecture ref: work-design.md §8.2

use serde::Serialize;
use std::time::Duration;

use crate::types::{TaskId, WorkOutcome};

/// Events that the Work System writes to the Trace Store for observability.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "trace_type", rename_all = "snake_case")]
pub enum WorkTraceEvent {
    /// 巡检开始。
    CheckStarted {
        candidates_count: usize,
    },
    /// 认领尝试。
    ClaimAttempted {
        task_id: TaskId,
        outcome: ClaimOutcome,
    },
    /// 步骤执行。
    StepExecuted {
        task_id: TaskId,
        step_index: usize,
        duration: Duration,
        success: bool,
        error: Option<String>,
    },
    /// 复核结果。
    ReviewCompleted {
        task_id: TaskId,
        passed: bool,
        confidence: f64,
    },
    /// 工作周期汇总。
    CycleCompleted {
        task_id: TaskId,
        outcome: WorkOutcome,
        total_duration: Duration,
        steps_completed: usize,
        steps_failed: usize,
    },
    /// 中断事件。
    Interrupted {
        task_id: Option<TaskId>,
        reason: String,
        by_system: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimOutcome {
    Success,
    TaskTakenByOther,
    PermissionDenied,
    BoardUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TaskId;

    #[test]
    fn work_trace_event_serde_tagged() {
        let event = WorkTraceEvent::CheckStarted {
            candidates_count: 5,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("check_started"), "expected tagged: {json}");
    }

    #[test]
    fn cycle_completed_serializes() {
        let event = WorkTraceEvent::CycleCompleted {
            task_id: TaskId::new(),
            outcome: WorkOutcome::Completed,
            total_duration: Duration::from_secs(42),
            steps_completed: 3,
            steps_failed: 0,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("cycle_completed"), "{json}");
    }
}
