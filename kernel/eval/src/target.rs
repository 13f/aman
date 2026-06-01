// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Evaluation target — what is being evaluated.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What is being evaluated.
///
/// Each variant represents a different kind of agent/LLM output that
/// can be scored by the evaluation engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvalTarget {
    /// An LLM's text response to a user query.
    LlmOutput {
        /// The full text content of the LLM response.
        content: String,
        /// The model that produced this output (e.g., "deepseek-v4-pro").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Which turn in the conversation this was.
        turn: u32,
        /// The user query that prompted this output (for context in LLM-as-judge).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
    },
    /// The result of a tool execution.
    ToolResult {
        /// Name of the tool that was called.
        tool_name: String,
        /// The input parameters passed to the tool.
        input: Value,
        /// The output returned by the tool.
        output: Value,
    },
    /// A completed task/work-item.
    TaskResult {
        /// Unique task identifier.
        task_id: String,
        /// Human-readable description of what the task was.
        description: String,
        /// The result produced by completing the task.
        result: Value,
    },
    /// The output of a pipeline step or entire pipeline.
    PipelineResult {
        /// Pipeline identifier.
        pipeline_id: String,
        /// Optional step name within the pipeline.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<String>,
        /// The pipeline output.
        output: Value,
    },
    /// A custom evaluation target with an arbitrary label and content.
    Custom {
        /// Short label describing what this target is.
        label: String,
        /// Arbitrary content to evaluate.
        content: Value,
    },
}

impl EvalTarget {
    /// Returns a concise kind label for matching against `EvalRule::applies_to`.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::LlmOutput { .. } => "llm_output",
            Self::ToolResult { .. } => "tool_result",
            Self::TaskResult { .. } => "task_result",
            Self::PipelineResult { .. } => "pipeline_result",
            Self::Custom { .. } => "custom",
        }
    }

    /// Returns a stable-ish identifier string for this target.
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::LlmOutput { turn, .. } => {
                format!("llm_output:turn_{turn}")
            }
            Self::ToolResult { tool_name, .. } => {
                format!("tool_result:{tool_name}")
            }
            Self::TaskResult { task_id, .. } => {
                format!("task_result:{task_id}")
            }
            Self::PipelineResult {
                pipeline_id, step, ..
            } => {
                if let Some(s) = step {
                    format!("pipeline_result:{pipeline_id}:{s}")
                } else {
                    format!("pipeline_result:{pipeline_id}")
                }
            }
            Self::Custom { label, .. } => {
                format!("custom:{label}")
            }
        }
    }

    /// Extract the primary text content from any target variant.
    /// Returns `None` for targets that don't have a meaningful text representation.
    #[must_use]
    pub fn text_content(&self) -> Option<&str> {
        match self {
            Self::LlmOutput { content, .. } => Some(content.as_str()),
            Self::ToolResult { output, .. } => output.as_str(),
            Self::TaskResult { result, .. } => result.as_str(),
            Self::PipelineResult { output, .. } => output.as_str(),
            Self::Custom { content, .. } => content.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_labels_match_variants() {
        assert_eq!(
            EvalTarget::LlmOutput {
                content: "hi".into(),
                model: None,
                turn: 1,
                query: None,
            }
            .kind(),
            "llm_output"
        );
        assert_eq!(
            EvalTarget::ToolResult {
                tool_name: "search".into(),
                input: Value::Null,
                output: Value::String("ok".into()),
            }
            .kind(),
            "tool_result"
        );
        assert_eq!(
            EvalTarget::Custom {
                label: "test".into(),
                content: Value::Null,
            }
            .kind(),
            "custom"
        );
    }

    #[test]
    fn text_content_extraction() {
        let t = EvalTarget::LlmOutput {
            content: "hello world".into(),
            model: None,
            turn: 1,
            query: None,
        };
        assert_eq!(t.text_content(), Some("hello world"));
    }
}
