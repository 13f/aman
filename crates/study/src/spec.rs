// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! StudySpec — implements [`lifecycle::SystemSpec`] for the study domain.

use lifecycle::{IdleSignal, LifecycleError, StepOutput, SystemSpec};

use crate::types::{StudyItem, StudyPhase};

/// Glue between the generic lifecycle engine and the study domain.
pub struct StudySpec {
    pub auto_decompose: bool,
}

impl StudySpec {
    #[must_use]
    pub fn new(auto_decompose: bool) -> Self {
        Self { auto_decompose }
    }
}

impl SystemSpec for StudySpec {
    type Item = StudyItem;
    type Step = StudyPhase;

    fn event_source() -> &'static str {
        "study.system"
    }

    fn step_event_kind() -> &'static str {
        "study.phase.execute"
    }

    fn assigned_kind() -> &'static str {
        "study.item.assigned"
    }

    fn completed_kind() -> &'static str {
        "study.item.completed"
    }

    fn failed_kind() -> &'static str {
        "study.item.failed"
    }

    fn interrupt_kind() -> &'static str {
        "study.interrupt"
    }

    fn item_id(item: &StudyItem) -> String {
        item.id.to_string()
    }

    fn notify_on_complete(item: &StudyItem) -> bool {
        item.notify_on_complete
    }

    fn serialize_item(item: &StudyItem) -> serde_json::Value {
        serde_json::to_value(item).unwrap_or_default()
    }

    fn make_assigned_payload(item: &StudyItem, source: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "study_event_type": "study_item_assigned",
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
            "study_event_type": "study_item_completed",
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
            "study_event_type": "study_item_failed",
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
        serde_json::json!({ "phase_index": step_index })
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

    fn default_step(item: &StudyItem, _max_retries: u32) -> StudyPhase {
        match item.depth {
            crate::types::StudyDepth::Skim => StudyPhase::GatherMaterials,
            _ => StudyPhase::Plan,
        }
    }

    fn step_max_retries(step: &StudyPhase) -> u32 {
        step.max_retries()
    }

    async fn decompose(&self, item: &StudyItem, _max_retries: u32) -> Vec<StudyPhase> {
        build_phase_pipeline(item)
    }

    async fn execute_step_impl(
        &self,
        _item: &StudyItem,
        phase: &StudyPhase,
        _step_index: usize,
    ) -> Result<StepOutput, LifecycleError> {
        // Placeholder: real integration would do LLM reasoning, tool calls, etc.
        let description = phase.description();
        Ok(StepOutput {
            success: true,
            summary: format!("Completed: {description}"),
            artifacts: Vec::new(),
            duration: std::time::Duration::from_millis(50),
        })
    }

    fn collect_result(_item: &StudyItem, outputs: &[StepOutput]) -> serde_json::Value {
        let all_ok = outputs.iter().all(|o| o.success);
        let comprehension = if all_ok { 0.85 } else { 0.4 };
        serde_json::json!({
            "comprehension": comprehension,
            "phases_completed": outputs.len(),
        })
    }

    fn completion_signal(_item: &StudyItem) -> IdleSignal {
        IdleSignal::Satisfaction {
            item_id: lifecycle::ItemId::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Phase pipeline builder
// ---------------------------------------------------------------------------

fn build_phase_pipeline(item: &StudyItem) -> Vec<StudyPhase> {
    build_for_depth(item)
}

fn build_for_depth(item: &StudyItem) -> Vec<StudyPhase> {
    let mut phases = Vec::new();

    // Always start with GatherMaterials if no materials provided.
    if item.materials.is_none() || item.materials.as_ref().is_some_and(|m| m.is_empty()) {
        phases.push(StudyPhase::GatherMaterials);
    }

    match item.depth {
        crate::types::StudyDepth::Skim => {
            // Skim: just GatherMaterials (single pass, no notes).
            if phases.is_empty() {
                phases.push(StudyPhase::GatherMaterials);
            }
        }
        crate::types::StudyDepth::Read => {
            phases.push(StudyPhase::Plan);
            // Assume 3 modules for placeholder.
            for i in 0..3 {
                phases.push(StudyPhase::LearnModule { index: i });
            }
            phases.push(StudyPhase::Consolidate);
        }
        crate::types::StudyDepth::Deep => {
            phases.push(StudyPhase::Plan);
            for i in 0..4 {
                phases.push(StudyPhase::LearnModule { index: i });
            }
            phases.push(StudyPhase::Practice);
            phases.push(StudyPhase::Consolidate);
        }
    }

    phases
}
