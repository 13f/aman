// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! DailySpec — implements [`lifecycle::SystemSpec`] for the daily-life domain.

use lifecycle::{IdleSignal, LifecycleError, StepOutput, SystemSpec};

use crate::types::{DailyItem, Routine, RoutineAction, RoutinePriority, TimeWindow};

pub struct DailySpec;

impl DailySpec {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for DailySpec {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemSpec for DailySpec {
    type Item = DailyItem;
    type Step = Routine;

    fn event_source() -> &'static str {
        "daily.system"
    }

    fn step_event_kind() -> &'static str {
        "daily.routine.execute"
    }

    fn assigned_kind() -> &'static str {
        "daily.item.assigned"
    }

    fn completed_kind() -> &'static str {
        "daily.item.completed"
    }

    fn failed_kind() -> &'static str {
        "daily.item.failed"
    }

    fn interrupt_kind() -> &'static str {
        "daily.interrupt"
    }

    fn item_id(item: &DailyItem) -> String {
        item.id.to_string()
    }

    fn notify_on_complete(item: &DailyItem) -> bool {
        item.notify_on_complete
    }

    fn serialize_item(item: &DailyItem) -> serde_json::Value {
        serde_json::to_value(item).unwrap_or_default()
    }

    fn make_assigned_payload(item: &DailyItem, source: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "daily_event_type": "daily_item_assigned",
            "item": serde_json::to_value(item).unwrap_or_default(),
            "source": source,
        })
    }

    fn make_completed_payload(
        item_id: &str,
        result: serde_json::Value,
        duration_secs: f64,
    ) -> serde_json::Value {
        serde_json::json!({
            "daily_event_type": "daily_item_completed",
            "item_id": item_id,
            "outcome": result,
            "duration": duration_secs,
        })
    }

    fn make_failed_payload(
        item_id: &str,
        error: &LifecycleError,
        retryable: bool,
    ) -> serde_json::Value {
        serde_json::json!({
            "daily_event_type": "daily_item_failed",
            "item_id": item_id,
            "error": {
                "code": error.code,
                "message": error.message,
                "retryable": error.retryable,
            },
            "retryable": retryable,
        })
    }

    fn make_step_payload(step_index: usize) -> serde_json::Value {
        serde_json::json!({ "routine_index": step_index })
    }

    fn make_result_notify(
        item_id: &str,
        result: &serde_json::Value,
        agent_id: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "item_id": item_id,
            "result": result,
            "agent_id": agent_id,
        })
    }

    fn make_failure_notify(item_id: &str, error_msg: &str, agent_id: &str) -> serde_json::Value {
        serde_json::json!({
            "item_id": item_id,
            "error": error_msg,
            "agent_id": agent_id,
        })
    }

    fn default_step(_item: &DailyItem, _max_retries: u32) -> Routine {
        Routine {
            name: "daily_brief".into(),
            action: RoutineAction::DailyBrief,
            priority: RoutinePriority::Standard,
        }
    }

    fn step_max_retries(step: &Routine) -> u32 {
        step.max_retries()
    }

    async fn decompose(&self, item: &DailyItem, _max_retries: u32) -> Vec<Routine> {
        if let Some(ref predefined) = item.routines {
            if !predefined.is_empty() {
                return predefined.clone();
            }
        }
        default_routines_for_window(item.window)
    }

    async fn execute_step_impl(
        &self,
        _item: &DailyItem,
        routine: &Routine,
        _step_index: usize,
    ) -> Result<StepOutput, LifecycleError> {
        let description = routine_action_summary(&routine.action);
        Ok(StepOutput {
            success: true,
            summary: format!("Completed: {description}"),
            artifacts: Vec::new(),
            duration: std::time::Duration::from_millis(50),
        })
    }

    fn collect_result(_item: &DailyItem, outputs: &[StepOutput]) -> serde_json::Value {
        let all_ok = outputs.iter().all(|o| o.success);
        serde_json::json!({
            "outcome": if all_ok { "completed" } else { "partial" },
            "routines_completed": outputs.len(),
        })
    }

    fn completion_signal(_item: &DailyItem) -> IdleSignal {
        IdleSignal::Satisfaction {
            item_id: lifecycle::ItemId::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Default routines per time window
// ---------------------------------------------------------------------------

fn default_routines_for_window(window: TimeWindow) -> Vec<Routine> {
    match window {
        TimeWindow::Morning => vec![
            Routine {
                name: "check_calendar".into(),
                action: RoutineAction::CheckCalendar { days_ahead: 1 },
                priority: RoutinePriority::Essential,
            },
            Routine {
                name: "check_weather".into(),
                action: RoutineAction::CheckWeather,
                priority: RoutinePriority::Standard,
            },
            Routine {
                name: "check_habits".into(),
                action: RoutineAction::CheckHabits,
                priority: RoutinePriority::Essential,
            },
            Routine {
                name: "daily_brief".into(),
                action: RoutineAction::DailyBrief,
                priority: RoutinePriority::Essential,
            },
        ],
        TimeWindow::Midday => vec![
            Routine {
                name: "check_habits".into(),
                action: RoutineAction::CheckHabits,
                priority: RoutinePriority::Standard,
            },
            Routine {
                name: "check_health".into(),
                action: RoutineAction::CheckHealth,
                priority: RoutinePriority::Optional,
            },
        ],
        TimeWindow::Evening => vec![
            Routine {
                name: "check_habits".into(),
                action: RoutineAction::CheckHabits,
                priority: RoutinePriority::Standard,
            },
            Routine {
                name: "check_health".into(),
                action: RoutineAction::CheckHealth,
                priority: RoutinePriority::Optional,
            },
        ],
        TimeWindow::Night => vec![
            Routine {
                name: "check_habits".into(),
                action: RoutineAction::CheckHabits,
                priority: RoutinePriority::Essential,
            },
            Routine {
                name: "evening_review".into(),
                action: RoutineAction::GuideReflection {
                    template: "evening_review".into(),
                },
                priority: RoutinePriority::Essential,
            },
        ],
        TimeWindow::Afternoon => vec![],
    }
}

fn routine_action_summary(action: &RoutineAction) -> String {
    match action {
        RoutineAction::CheckCalendar { days_ahead } => format!("Check calendar ({days_ahead}d ahead)"),
        RoutineAction::CheckWeather => "Check weather".into(),
        RoutineAction::CheckHabits => "Check habits".into(),
        RoutineAction::CheckHealth => "Check health".into(),
        RoutineAction::GuideReflection { template } => format!("Guide reflection ({template})"),
        RoutineAction::DailyBrief => "Generate daily brief".into(),
        RoutineAction::CustomPrompt { prompt } => format!("Custom: {prompt}"),
    }
}
