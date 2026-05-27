// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! WorkTraceEvent — events written to the Agent's private Trace Store.
//!
//! Architecture ref: work-design.md v2 §8.2

use serde::Serialize;
use std::time::Duration;

use crate::types::WorkItemId;

/// Events that the Work System writes to the Trace Store for observability.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "trace_type", rename_all = "snake_case")]
pub enum WorkTraceEvent {
    /// A work item was received and enqueued.
    ItemReceived {
        item_id: WorkItemId,
        source: String,
    },
    /// A step was executed.
    StepExecuted {
        item_id: WorkItemId,
        step_index: usize,
        duration: Duration,
        success: bool,
        error: Option<String>,
    },
    /// A work item completed successfully.
    ItemCompleted {
        item_id: WorkItemId,
        duration: Duration,
        steps_completed: usize,
        steps_failed: usize,
    },
    /// A work item failed.
    ItemFailed {
        item_id: WorkItemId,
        error: String,
        retryable: bool,
    },
    /// Execution was interrupted.
    Interrupted {
        item_id: Option<WorkItemId>,
        reason: String,
        by_system: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_trace_event_serde_tagged() {
        let event = WorkTraceEvent::ItemReceived {
            item_id: WorkItemId::new(),
            source: "cli".into(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("item_received"), "expected tagged: {json}");
    }

    #[test]
    fn item_completed_serializes() {
        let event = WorkTraceEvent::ItemCompleted {
            item_id: WorkItemId::new(),
            duration: Duration::from_secs(42),
            steps_completed: 3,
            steps_failed: 0,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("item_completed"), "{json}");
    }

    #[test]
    fn item_failed_serializes() {
        let event = WorkTraceEvent::ItemFailed {
            item_id: WorkItemId::new(),
            error: "timeout".into(),
            retryable: true,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("item_failed"), "{json}");
    }
}
