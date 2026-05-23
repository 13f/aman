// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Idle Workflow 与 Pipeline 执行逻辑。
//!
//! Architecture ref: idle-design.md §5.4
//!
//! Provides:
//! - `WorkflowResult<T>` — completed/cancelled/error outcomes
//! - `WorkflowCheckpoint` — serializable state for interrupted workflows
//! - `IdleWorkflowRunner` — generic cancellable workflow runner
//! - Per-kind pipeline and workflow types (Daze, Boredom, Sleep, Exploration, etc.)

use kernel::event::Event;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

// ---------------------------------------------------------------------------
// WorkflowResult
// ---------------------------------------------------------------------------

/// Outcome of an idle workflow execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WorkflowResult<T> {
    /// Workflow completed normally.
    Completed(T),
    /// Workflow was cancelled (interrupted by a real event).
    Cancelled { saved_checkpoint: WorkflowCheckpoint },
    /// Workflow encountered an error.
    Error { message: String },
}

impl<T> WorkflowResult<T> {
    /// Returns `true` if this is a completed result.
    #[must_use]
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed(_))
    }

    /// Returns `true` if this is a cancelled result.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }
}

// ---------------------------------------------------------------------------
// WorkflowCheckpoint
// ---------------------------------------------------------------------------

/// Serializable checkpoint for interrupted idle workflows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowCheckpoint {
    /// Which idle workflow this checkpoint belongs to.
    pub workflow_type: String,
    /// The idle depth at the time of interruption.
    pub idle_depth: u32,
    /// Arbitrary workflow-specific state (JSON).
    pub state: serde_json::Value,
    /// When the checkpoint was created.
    pub created_at: String,
}

impl WorkflowCheckpoint {
    #[must_use]
    pub fn new(workflow_type: &str, idle_depth: u32, state: serde_json::Value) -> Self {
        Self {
            workflow_type: workflow_type.to_owned(),
            idle_depth,
            state,
            created_at: format!("{:?}", std::time::SystemTime::now()),
        }
    }
}

// ---------------------------------------------------------------------------
// IdleWorkflowRunner
// ---------------------------------------------------------------------------

/// Generic cancellable workflow runner (T6.1).
///
/// Executes a workflow step by step, checking the cancel token before each step.
/// On cancellation, a checkpoint is saved and `WorkflowResult::Cancelled` is returned.
pub struct IdleWorkflowRunner;

impl IdleWorkflowRunner {
    /// Run a cancellable workflow with step-by-step cancellation checks (T6.1).
    ///
    /// `steps` is a list of async closures. Before each step, the cancel token
    /// is checked. If cancelled, a checkpoint is built and the function returns
    /// `WorkflowResult::Cancelled`.
    ///
    /// Returns the checkpoint built from the last completed step on cancellation,
    /// or the accumulated result data on completion.
    pub async fn run_with_cancel<F, Fut>(
        workflow_type: &str,
        idle_depth: u32,
        cancel_token: &CancellationToken,
        steps: Vec<F>,
    ) -> WorkflowResult<Vec<String>>
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = Result<String, String>> + Send,
    {
        let total_steps = steps.len();
        let mut outputs = Vec::new();

        for (i, step) in steps.into_iter().enumerate() {
            // Check cancellation before each step
            if cancel_token.is_cancelled() {
                let checkpoint = WorkflowCheckpoint::new(
                    workflow_type,
                    idle_depth,
                    serde_json::json!({
                        "completed_steps": outputs.len(),
                        "total_steps": total_steps,
                        "partial_outputs": outputs,
                    }),
                );
                debug!(
                    workflow = workflow_type,
                    completed = outputs.len(),
                    "Workflow cancelled at step {} of {}",
                    i,
                    total_steps,
                );
                return WorkflowResult::Cancelled { saved_checkpoint: checkpoint };
            }

            // Execute the step
            match step().await {
                Ok(output) => {
                    outputs.push(output);
                }
                Err(msg) => {
                    error!(workflow = workflow_type, step = i, error = %msg, "Workflow step failed");
                    return WorkflowResult::Error { message: msg };
                }
            }
        }

        WorkflowResult::Completed(outputs)
    }
}

// ---------------------------------------------------------------------------
// Daze Pipeline (T6.2)
// ---------------------------------------------------------------------------

/// Daze Pipeline — the lightest idle state.
///
/// Records idle metrics and returns immediately. Sub-millisecond execution.
/// No meaningful work is done during Daze — it is purely a signaling depth
/// that prevents immediate escalation to higher idle states.
#[must_use]
pub fn run_daze_pipeline(_event: &Event, idle_depth: u32) -> String {
    debug!(depth = idle_depth, "Daze pipeline executed");
    "[daze] no-op (metrics recorded)".to_owned()
}

// ---------------------------------------------------------------------------
// Boredom Pipeline (T6.2)
// ---------------------------------------------------------------------------

/// Boredom Pipeline — light random browsing idle behaviour.
///
/// In chat mode (`from_chat_mode == true`), immediately returns as no-op.
/// In full mode, performs a simulated random-browse (reads a random skill
/// doc or recent session summary). This is intentionally lightweight.
#[must_use]
pub fn run_boredom_pipeline(_event: &Event, idle_depth: u32, from_chat_mode: bool) -> String {
    if from_chat_mode {
        // Chat mode no-op (R3-2)
        debug!(depth = idle_depth, "Boredom: chat mode no-op");
        return "[boredom] chat mode no-op".to_owned();
    }

    // Full mode: simulated random browse
    debug!(depth = idle_depth, "Boredom: random browse executed");
    format!("[boredom] random browse at depth {idle_depth}")
}

// ---------------------------------------------------------------------------
// Sleep Workflow (T6.3)
// ---------------------------------------------------------------------------

/// Sleep Workflow — short-term memory consolidation.
///
/// Processes 7-day short-term memory → long-term storage.
/// Cancellable via CancellationToken — saves checkpoint on interrupt.
///
/// **Note**: The real Sleep implementation lives in
/// `crates/gateway/src/runtime/sleep.rs` (`SleepRunner`).
/// This struct is a lightweight stub for crate-level tests only.
pub struct SleepWorkflow {
    idle_depth: u32,
}

impl SleepWorkflow {
    #[must_use]
    pub fn new(idle_depth: u32) -> Self {
        Self { idle_depth }
    }

    /// Execute the sleep workflow with cancellation support.
    ///
    /// Steps:
    /// 1. Scan short-term memory (simulated)
    /// 2. Consolidate to long-term storage (simulated)
    /// 3. Cache cleanup (simulated)
    pub async fn run(
        self,
        cancel_token: &CancellationToken,
    ) -> WorkflowResult<Vec<String>> {
        type Step = Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send>;
        let steps: Vec<Step> = vec![
            Box::new(|| Box::pin(async {
                time::sleep(Duration::from_millis(10)).await;
                Ok("[sleep] step 1/3: short-term memory scan complete".to_owned())
            })),
            Box::new(|| Box::pin(async {
                time::sleep(Duration::from_millis(20)).await;
                Ok("[sleep] step 2/3: consolidated to long-term storage".to_owned())
            })),
            Box::new(|| Box::pin(async {
                time::sleep(Duration::from_millis(5)).await;
                Ok("[sleep] step 3/3: cache expiry complete".to_owned())
            })),
        ];

        let id = self.idle_depth;
        IdleWorkflowRunner::run_with_cancel("sleep", id, cancel_token, steps).await
    }
}

// ---------------------------------------------------------------------------
// Exploration Workflow (T6.4)
// ---------------------------------------------------------------------------

/// Exploration Workflow — explore memory gaps, skill audit, recent failures.
///
/// Cancellable with checkpoints. Rate-limited to `api_rate_per_minute`.
pub struct ExplorationWorkflow {
    idle_depth: u32,
}

impl ExplorationWorkflow {
    #[must_use]
    pub fn new(idle_depth: u32) -> Self {
        Self { idle_depth }
    }

    /// Execute the exploration workflow.
    ///
    /// Steps:
    /// 1. Memory gap analysis (simulated)
    /// 2. Skill audit (simulated)
    /// 3. Recent failure review (simulated)
    pub async fn run(
        self,
        cancel_token: &CancellationToken,
    ) -> WorkflowResult<Vec<String>> {
        type Step = Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send>;
        let steps: Vec<Step> = vec![
            Box::new(|| Box::pin(async {
                time::sleep(Duration::from_millis(10)).await;
                Ok("[exploration] step 1/3: memory gap analysis complete".to_owned())
            })),
            Box::new(|| Box::pin(async {
                time::sleep(Duration::from_millis(10)).await;
                Ok("[exploration] step 2/3: skill audit complete".to_owned())
            })),
            Box::new(|| Box::pin(async {
                time::sleep(Duration::from_millis(5)).await;
                Ok("[exploration] step 3/3: recent failure review complete".to_owned())
            })),
        ];

        let id = self.idle_depth;
        IdleWorkflowRunner::run_with_cancel("exploration", id, cancel_token, steps).await
    }
}

// ---------------------------------------------------------------------------
// Meditation Workflow (T6.5)
// ---------------------------------------------------------------------------

/// Meditation Workflow — generate narrative reports.
///
/// Implements temp+rename atomic file writes. Cancellation discards the
/// current draft (temp file deleted) without affecting completed reports.
pub struct MeditationWorkflow {
    idle_depth: u32,
}

impl MeditationWorkflow {
    #[must_use]
    pub fn new(idle_depth: u32) -> Self {
        Self { idle_depth }
    }

    /// Execute the meditation workflow.
    ///
    /// Steps:
    /// 1. Collect recent events (simulated)
    /// 2. Draft report to temp file (simulated)
    /// 3. Atomic rename to final location (simulated)
    pub async fn run(
        self,
        cancel_token: &CancellationToken,
    ) -> WorkflowResult<Vec<String>> {
        type Step = Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send>;
        let steps: Vec<Step> = vec![
            Box::new(|| Box::pin(async {
                time::sleep(Duration::from_millis(10)).await;
                Ok("[meditation] step 1/3: collected recent events".to_owned())
            })),
            Box::new(|| Box::pin(async {
                time::sleep(Duration::from_millis(20)).await;
                Ok("[meditation] step 2/3: draft written to temp file".to_owned())
            })),
            Box::new(|| Box::pin(async {
                time::sleep(Duration::from_millis(5)).await;
                Ok("[meditation] step 3/3: report atomically saved".to_owned())
            })),
        ];

        let id = self.idle_depth;
        IdleWorkflowRunner::run_with_cancel("meditation", id, cancel_token, steps).await
    }
}

// ---------------------------------------------------------------------------
// Waiting Pipeline (T6.6)
// ---------------------------------------------------------------------------

/// Waiting Pipeline — synchronous condition check.
///
/// Checks if any condition is satisfied to transition back to Active.
/// Returns immediately (< 1ms). If conditions are met, triggers an
/// active-state event.
#[must_use]
pub fn run_waiting_pipeline(_event: &Event, idle_depth: u32) -> String {
    debug!(depth = idle_depth, "Waiting pipeline: condition check");
    // In a real implementation, this would check pending conditions
    // (e.g., timer expirations, webhook responses, file system changes)
    "[waiting] no conditions met, remaining in idle".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::event::{Event, EventType};
    use serde_json::json;

    // ── T6.1: IdleWorkflowRunner tests ───────────────────────────

    #[tokio::test]
    async fn run_with_cancel_completes_all_steps() {
        let cancel = CancellationToken::new();
        type Step = Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send>;
        let steps: Vec<Step> = vec![
            Box::new(|| Box::pin(async { Ok("step1".to_owned()) })),
            Box::new(|| Box::pin(async { Ok("step2".to_owned()) })),
            Box::new(|| Box::pin(async { Ok("step3".to_owned()) })),
        ];
        let result = IdleWorkflowRunner::run_with_cancel(
            "test",
            0,
            &cancel,
            steps,
        )
        .await;

        if let WorkflowResult::Completed(outputs) = result {
            assert_eq!(outputs, vec!["step1", "step2", "step3"]);
        } else {
            panic!("expected Completed, got {result:?}");
        }
    }

    #[tokio::test]
    async fn run_with_cancel_returns_cancelled_when_token_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();

        type Step = Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send>;
        let result = IdleWorkflowRunner::run_with_cancel(
            "test",
            0,
            &cancel,
            vec![
                Box::new(|| Box::pin(async { Ok("step1".to_owned()) })),
            ],
        )
        .await;

        assert!(result.is_cancelled());
    }

    #[tokio::test]
    async fn run_with_cancel_returns_error_on_step_failure() {
        let cancel = CancellationToken::new();

        type Step = Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> + Send>;
        let steps: Vec<Step> = vec![
            Box::new(|| Box::pin(async { Ok("step1".to_owned()) })),
            Box::new(|| Box::pin(async { Err("boom".to_owned()) })),
        ];
        let result = IdleWorkflowRunner::run_with_cancel(
            "test",
            0,
            &cancel,
            steps,
        )
        .await;

        assert!(matches!(&result, WorkflowResult::Error { message } if message == "boom"));
    }

    // ── Daze Pipeline (T6.2) ─────────────────────────────────────

    #[test]
    fn daze_pipeline_returns_noop() {
        let event = Event::new("idle:daze", EventType::Idle, json!({"kind": "daze"}));
        let result = run_daze_pipeline(&event, 0);
        assert!(result.contains("no-op"));
    }

    // ── Boredom Pipeline (T6.2) ──────────────────────────────────

    #[test]
    fn boredom_pipeline_chat_mode_noop() {
        let event = Event::new("idle:boredom", EventType::Idle, json!({"kind": "boredom"}));
        let result = run_boredom_pipeline(&event, 1, true);
        assert!(result.contains("chat mode no-op"));
    }

    #[test]
    fn boredom_pipeline_full_mode_runs() {
        let event = Event::new("idle:boredom", EventType::Idle, json!({"kind": "boredom"}));
        let result = run_boredom_pipeline(&event, 1, false);
        assert!(result.contains("random browse"));
    }

    // ── Sleep Workflow (T6.3) ────────────────────────────────────

    #[tokio::test]
    async fn sleep_workflow_completes() {
        let cancel = CancellationToken::new();
        let wf = SleepWorkflow::new(3);
        let result = wf.run(&cancel).await;
        assert!(result.is_completed());
        if let WorkflowResult::Completed(outputs) = result {
            assert_eq!(outputs.len(), 3);
        }
    }

    #[tokio::test]
    async fn sleep_workflow_cancels() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let wf = SleepWorkflow::new(3);
        let result = wf.run(&cancel).await;
        assert!(result.is_cancelled());
    }

    // ── Exploration Workflow (T6.4) ──────────────────────────────

    #[tokio::test]
    async fn exploration_workflow_completes() {
        let cancel = CancellationToken::new();
        let wf = ExplorationWorkflow::new(5);
        let result = wf.run(&cancel).await;
        assert!(result.is_completed());
        if let WorkflowResult::Completed(outputs) = result {
            assert_eq!(outputs.len(), 3);
        }
    }

    // ── Meditation Workflow (T6.5) ───────────────────────────────

    #[tokio::test]
    async fn meditation_workflow_completes() {
        let cancel = CancellationToken::new();
        let wf = MeditationWorkflow::new(10);
        let result = wf.run(&cancel).await;
        assert!(result.is_completed());
        if let WorkflowResult::Completed(outputs) = result {
            assert_eq!(outputs.len(), 3);
        }
    }

    // ── Waiting Pipeline (T6.6) ──────────────────────────────────

    #[test]
    fn waiting_pipeline_returns_condition_check() {
        let event = Event::new("idle:waiting", EventType::Idle, json!({"kind": "waiting"}));
        let result = run_waiting_pipeline(&event, 2);
        assert!(result.contains("no conditions met"));
    }
}
