// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! DailyTraceEvent — observability events for the daily-life system.

use serde::Serialize;
use std::time::Duration;

use crate::types::{DailyItemId, TimeWindow};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "trace_type", rename_all = "snake_case")]
pub enum DailyTraceEvent {
    ItemReceived {
        item_id: DailyItemId,
        window: TimeWindow,
        source: String,
    },
    RoutineExecuted {
        item_id: DailyItemId,
        routine_name: String,
        duration: Duration,
        success: bool,
        error: Option<String>,
    },
    ItemCompleted {
        item_id: DailyItemId,
        duration: Duration,
        routines_completed: usize,
    },
    ItemFailed {
        item_id: DailyItemId,
        error: String,
        retryable: bool,
    },
    Interrupted {
        reason: String,
        by_system: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_event_serde_tagged() {
        let event = DailyTraceEvent::ItemReceived {
            item_id: DailyItemId::new(),
            window: TimeWindow::Morning,
            source: "cron".into(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("item_received"), "{json}");
    }

    #[test]
    fn item_completed_serializes() {
        let event = DailyTraceEvent::ItemCompleted {
            item_id: DailyItemId::new(),
            duration: Duration::from_secs(30),
            routines_completed: 4,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("item_completed"), "{json}");
    }
}
