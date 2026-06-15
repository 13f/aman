// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! WorkSpec — implements [`lifecycle::SystemSpec`] for the work domain.

use lifecycle::{IdleSignal, LifecycleError, StepOutput, SystemSpec};

use crate::types::{Step, WorkItem};

/// Glue between the generic lifecycle engine and the work domain.
pub struct WorkSpec {
    pub auto_decompose: bool,
}

impl WorkSpec {
    #[must_use]
    pub fn new(auto_decompose: bool) -> Self {
        Self { auto_decompose }
    }
}

impl SystemSpec for WorkSpec {
    type Item = WorkItem;
    type Step = Step;

    fn event_source() -> &'static str {
        "work.system"
    }

    fn step_event_kind() -> &'static str {
        "work.step.execute"
    }

    fn assigned_kind() -> &'static str {
        "work.item.assigned"
    }

    fn completed_kind() -> &'static str {
        "work.item.completed"
    }

    fn failed_kind() -> &'static str {
        "work.item.failed"
    }

    fn interrupt_kind() -> &'static str {
        "work.interrupt"
    }

    fn item_id(item: &WorkItem) -> String {
        item.id.to_string()
    }

    fn notify_on_complete(item: &WorkItem) -> bool {
        item.notify_on_complete
    }

    fn serialize_item(item: &WorkItem) -> serde_json::Value {
        serde_json::to_value(item).unwrap_or_default()
    }

    fn make_assigned_payload(item: &WorkItem, source: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "work_event_type": "work_item_assigned",
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
            "work_event_type": "work_item_completed",
            "item_id": item_id,
            "result": result,
            "duration": duration_secs,
        })
    }

    fn make_failed_payload(
        item_id: &str,
        error: &LifecycleError,
        retryable: bool,
    ) -> serde_json::Value {
        serde_json::json!({
            "work_event_type": "work_item_failed",
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
        serde_json::json!({
            "step_index": step_index,
        })
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

    fn default_step(_item: &WorkItem, max_retries: u32) -> Step {
        Step {
            index: 0,
            description: "Execute work item".into(),
            tool: None,
            expect_llm: true,
            max_retries,
        }
    }

    fn step_max_retries(step: &Step) -> u32 {
        step.max_retries
    }

    async fn decompose(&self, item: &WorkItem, max_retries: u32) -> Vec<Step> {
        // Use predefined steps if present.
        if let Some(ref predefined) = item.steps
            && !predefined.is_empty()
        {
            return predefined.clone();
        }

        if !self.auto_decompose {
            return vec![];
        }

        // LLM decomposition (placeholder).
        let mut steps = Vec::new();
        steps.push(Step {
            index: 0,
            description: format!("Analyze: {}", item.title),
            tool: None,
            expect_llm: true,
            max_retries: 1,
        });

        let desc_lower = item.description.to_lowercase();
        if desc_lower.contains("code")
            || desc_lower.contains("fix")
            || desc_lower.contains("refactor")
            || desc_lower.contains("implement")
        {
            steps.push(Step {
                index: steps.len(),
                description: format!("Implement: {}", item.title),
                tool: Some("file".into()),
                expect_llm: true,
                max_retries,
            });
        }
        if desc_lower.contains("test")
            || desc_lower.contains("review")
            || desc_lower.contains("verify")
        {
            steps.push(Step {
                index: steps.len(),
                description: format!("Verify: {}", item.title),
                tool: Some("exec".into()),
                expect_llm: false,
                max_retries: 1,
            });
        }

        steps.push(Step {
            index: steps.len(),
            description: format!("Finalize: {}", item.title),
            tool: None,
            expect_llm: true,
            max_retries: 1,
        });

        steps
    }

    async fn execute_step_impl(
        &self,
        _item: &WorkItem,
        step: &Step,
        _step_index: usize,
    ) -> Result<StepOutput, LifecycleError> {
        // Placeholder: real integration calls LLM/tool.
        Ok(StepOutput {
            success: true,
            summary: format!("Completed: {}", step.description),
            artifacts: Vec::new(),
            duration: std::time::Duration::from_millis(50),
        })
    }

    fn collect_result(_item: &WorkItem, outputs: &[StepOutput]) -> serde_json::Value {
        let steps_completed = outputs.iter().filter(|o| o.success).count();
        let steps_failed = outputs.iter().filter(|o| !o.success).count();
        serde_json::json!({
            "outcome": "completed",
            "summary": format!("Completed {} steps ({} ok, {} failed)", outputs.len(), steps_completed, steps_failed),
            "steps_completed": steps_completed,
            "steps_failed": steps_failed,
        })
    }

    fn completion_signal(_item: &WorkItem) -> IdleSignal {
        IdleSignal::Satisfaction {
            item_id: lifecycle::ItemId::new(),
        }
    }
}
