// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! StudyTraceEvent — observability events for the study system.

use serde::Serialize;
use std::time::Duration;

use crate::types::StudyItemId;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "trace_type", rename_all = "snake_case")]
pub enum StudyTraceEvent {
    ItemReceived {
        item_id: StudyItemId,
        topic: String,
        source: String,
    },
    PhaseExecuted {
        item_id: StudyItemId,
        phase: String,
        duration: Duration,
        success: bool,
        error: Option<String>,
    },
    ItemCompleted {
        item_id: StudyItemId,
        duration: Duration,
        comprehension: f64,
    },
    ItemFailed {
        item_id: StudyItemId,
        error: String,
        retryable: bool,
    },
    Interrupted {
        item_id: Option<StudyItemId>,
        reason: String,
        by_system: String,
    },
    ReviewScheduled {
        item_id: StudyItemId,
        next_review_days: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_event_serde_tagged() {
        let event = StudyTraceEvent::ItemReceived {
            item_id: StudyItemId::new(),
            topic: "Rust async".into(),
            source: "user".into(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("item_received"), "{json}");
    }

    #[test]
    fn item_completed_serializes() {
        let event = StudyTraceEvent::ItemCompleted {
            item_id: StudyItemId::new(),
            duration: Duration::from_secs(120),
            comprehension: 0.85,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("item_completed"), "{json}");
    }

    #[test]
    fn review_scheduled_serializes() {
        let event = StudyTraceEvent::ReviewScheduled {
            item_id: StudyItemId::new(),
            next_review_days: 7,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("review_scheduled"), "{json}");
    }
}
