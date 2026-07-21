// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Experience extractor — subscribes to workflow::completed events and
//! updates EXP.md with tool strategy outcomes.
//!
//! The extractor runs in the background, listening for workflow completion
//! events. When a workflow finishes, it reads the current EXP.md, finds or
//! creates the matching strategy entry, and updates success/failure counts.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use kernel::error::AmanResult;
use kernel::event::Event;
use event_bus::EventHandler;

use super::agent_seed::aman_data_dir;

/// Determines whether a workflow final state represents success.
const SUCCESS_STATES: &[&str] = &["approved", "completed", "success", "done", "finished"];
const FAILURE_STATES: &[&str] = &["rejected", "failed", "error", "cancelled", "timeout"];

/// Experience extractor — subscribes to workflow completion events.
pub struct ExperienceExtractor {
    /// Agent ID this extractor serves.
    agent_id: String,
    /// Workflow engine reference for querying instance data.
    /// Currently unused — kept for future richer extraction (tool-level details).
    #[allow(dead_code)]
    workflow_engine: Arc<workflow::WorkflowEngine>,
}

impl ExperienceExtractor {
    pub fn new(
        agent_id: impl Into<String>,
        workflow_engine: Arc<workflow::WorkflowEngine>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            workflow_engine,
        }
    }

    /// Path to this agent's EXP.md file.
    fn exp_md_path(&self) -> PathBuf {
        aman_data_dir()
            .join("agents")
            .join(&self.agent_id)
            .join("EXP.md")
    }

    /// Process a workflow::completed event.
    async fn handle_workflow_completed(&self, payload: &Value) -> AmanResult<()> {
        let instance_id = payload
            .get("instance_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let workflow_name = payload
            .get("workflow_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let to_state = payload
            .get("to_state")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let _reason = payload
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Determine outcome
        let success = self.is_success(to_state);

        // Update EXP.md
        self.record_outcome(workflow_name, success, instance_id).await;

        tracing::info!(
            agent_id = %self.agent_id,
            workflow = workflow_name,
            state = to_state,
            success,
            "experience extracted from workflow completion"
        );

        Ok(())
    }

    /// Determine whether a final state represents success.
    fn is_success(&self, state: &str) -> bool {
        let state = state.to_lowercase();
        if SUCCESS_STATES.iter().any(|s| state.contains(s)) {
            return true;
        }
        if FAILURE_STATES.iter().any(|s| state.contains(s)) {
            return false;
        }
        // Unknown state — treat as success if not explicitly failure
        true
    }

    /// Record an outcome in EXP.md.
    async fn record_outcome(&self, workflow_name: &str, success: bool, instance_id: &str) {
        let path = self.exp_md_path();

        // Read current EXP.md (or start fresh)
        let mut exp = if path.exists() {
            match experience::exp_md::parse_file(&path) {
                Ok(exp) => exp,
                Err(e) => {
                    tracing::warn!(
                        agent_id = %self.agent_id,
                        error = %e,
                        "failed to parse EXP.md, starting fresh"
                    );
                    experience::model::ExpMd::empty()
                }
            }
        } else {
            experience::model::ExpMd::empty()
        };

        // Find or create the strategy entry
        let tag = experience::model::ExperienceTag::new(workflow_name);

        if let Some(pos) = exp.strategies.iter().position(|e| e.tag == tag) {
            // Update existing entry
            let entry = &mut exp.strategies[pos];
            entry.uses += 1;
            if success {
                entry.successes += 1;
            }
            entry.confidence = entry.pattern_score();
            if !instance_id.is_empty() && !entry.learned_from.contains(&instance_id.to_string()) {
                entry.learned_from.push(instance_id.to_string());
            }
        } else {
            // Create new entry
            let mut new_entry = experience::model::ExperienceEntry {
                category: experience::model::ExperienceKind::ToolStrategy,
                tag: tag.clone(),
                description: format!("Workflow '{}' execution", workflow_name),
                content: format!("Auto-extracted from workflow '{}'", workflow_name),
                confidence: if success { 1.0 } else { 0.0 },
                uses: 1,
                successes: if success { 1 } else { 0 },
                needs_verification: false,
                learned_from: if instance_id.is_empty() {
                    vec![]
                } else {
                    vec![instance_id.to_string()]
                },
            };
            new_entry.confidence = new_entry.pattern_score();
            exp.strategies.push(new_entry);
        }

        // Write back
        if let Err(e) = experience::exp_md::write_file(&path, &exp) {
            tracing::error!(
                agent_id = %self.agent_id,
                error = %e,
                "failed to write EXP.md"
            );
        }
    }
}

#[async_trait::async_trait]
impl EventHandler for ExperienceExtractor {
    async fn handle(&self, event: Event) -> AmanResult<()> {
        if let Value::Object(_) = &event.payload
            && let Err(e) = self.handle_workflow_completed(&event.payload).await
        {
            tracing::warn!(
                agent_id = %self.agent_id,
                error = %e,
                "failed to process workflow::completed event"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_success() {
        let extractor = ExperienceExtractor::new("test", Arc::new(workflow::WorkflowEngine::new()));
        assert!(extractor.is_success("APPROVED"));
        assert!(extractor.is_success("completed"));
        assert!(extractor.is_success("success"));
        assert!(!extractor.is_success("FAILED"));
        assert!(!extractor.is_success("rejected"));
        assert!(!extractor.is_success("error"));
        assert!(!extractor.is_success("cancelled"));
    }

    #[test]
    fn test_is_success_unknown() {
        let extractor = ExperienceExtractor::new("test", Arc::new(workflow::WorkflowEngine::new()));
        // Unknown states default to success
        assert!(extractor.is_success("some_custom_state"));
    }

    #[test]
    fn test_exp_md_path() {
        let extractor = ExperienceExtractor::new("my-agent-123", Arc::new(workflow::WorkflowEngine::new()));
        let path = extractor.exp_md_path();
        assert!(path.to_string_lossy().contains("agents/my-agent-123/EXP.md"));
    }
}
