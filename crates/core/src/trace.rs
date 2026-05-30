// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! TraceStore — persistent task-level execution traces for idle cognitive
//! processing (Meditation pattern extraction, Reflection error analysis).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use crate::AmanResult;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Outcome of a traced task execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceOutcome {
    Success,
    Failure,
    Partial,
    Cancelled,
}

/// A single decision point recorded during task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionPoint {
    /// Human-readable description of the decision context.
    pub branch: String,
    /// The path actually chosen.
    pub taken: String,
    /// Paths considered but not taken.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    /// When the decision was made (UNIX ms).
    pub timestamp_ms: i64,
}

/// A tool invocation recorded during task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub tool_name: String,
    /// Abbreviated parameter summary.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub input_summary: String,
    /// Abbreviated result summary.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output_summary: String,
    pub duration_ms: u64,
    pub success: bool,
}

/// An error encountered during task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceError {
    /// Error class or code (e.g. "TimeoutError", "E_CONN_REFUSED").
    pub error_type: String,
    pub error_message: String,
    /// Recovery action taken, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_action: Option<String>,
    /// Whether the error was successfully recovered.
    pub recovered: bool,
}

/// A complete task execution trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceRecord {
    // ── Identity ──
    pub trace_id: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    // ── Task metadata ──
    pub task_type: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub input: String,
    pub outcome: TraceOutcome,
    pub duration_ms: u64,

    // ── Decision path ──
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_points: Vec<DecisionPoint>,

    // ── Tool call chain ──
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallRecord>,

    // ── Errors & recovery ──
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<TraceError>,

    // ── Entity references ──
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<String>,

    // ── Timestamps ──
    pub started_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<i64>,
}

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Aggregate statistics across all stored traces.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TraceStatsSummary {
    pub total_traces: u64,
    pub total_agents: u64,
    pub success_count: u64,
    pub failure_count: u64,
    pub partial_count: u64,
    pub cancelled_count: u64,
    pub total_errors: u64,
    pub total_tool_calls: u64,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Persistent storage for task-level execution traces.
///
/// Lifecycle: `begin_trace` → zero or more `append_*` calls → `end_trace`.
///
/// **Phase A** — data model + basic CRUD (begin/end_trace, load_recent, is_empty).
/// **Phase B** — decision points + error fields + load_recent_errors.
/// **Phase C** — chain detection + tool call fields.
/// **Phase D** — prune, complete management API.
///
/// Every method has a default no-op body so providers can implement
/// incrementally (same pattern as [`super::memory::MemoryProvider`]).
#[async_trait]
pub trait TraceStore: Send + Sync {
    /// Provider identifier.
    fn name(&self) -> &str;

    // ── Phase A: basic CRUD ──────────────────────────────────────────────

    /// Persist a complete trace record (convenience — combines begin + end).
    async fn save_trace(&self, trace: &TraceRecord) -> AmanResult<()> {
        let _ = trace;
        unimplemented!("TraceStore::save_trace")
    }

    /// Start a new trace. Writes a partial record immediately so it survives
    /// crashes. Returns the generated `trace_id`.
    async fn begin_trace(
        &self,
        agent_id: &str,
        session_id: Option<&str>,
        task_type: &str,
        description: &str,
        input: &str,
    ) -> AmanResult<String> {
        let _ = (agent_id, session_id, task_type, description, input);
        unimplemented!("TraceStore::begin_trace")
    }

    /// Finalize a trace with an outcome and entity list.
    async fn end_trace(
        &self,
        agent_id: &str,
        trace_id: &str,
        outcome: TraceOutcome,
        entities: &[String],
    ) -> AmanResult<()> {
        let _ = (agent_id, trace_id, outcome, entities);
        unimplemented!("TraceStore::end_trace")
    }

    /// Load the most recent `count` traces for the given agent, newest first.
    async fn load_recent(&self, agent_id: &str, count: usize) -> AmanResult<Vec<TraceRecord>> {
        let _ = (agent_id, count);
        unimplemented!("TraceStore::load_recent")
    }

    /// Load all traces associated with a session.
    async fn load_by_session(
        &self,
        agent_id: &str,
        session_id: &str,
    ) -> AmanResult<Vec<TraceRecord>> {
        let _ = (agent_id, session_id);
        unimplemented!("TraceStore::load_by_session")
    }

    /// Whether this agent has zero stored traces.
    async fn is_empty(&self, agent_id: &str) -> AmanResult<bool> {
        let _ = agent_id;
        unimplemented!("TraceStore::is_empty")
    }

    // ── Phase B: decision points + errors ────────────────────────────────

    /// Append a decision point to an in-progress trace.
    async fn append_decision_point(
        &self,
        agent_id: &str,
        trace_id: &str,
        dp: &DecisionPoint,
    ) -> AmanResult<()> {
        let _ = (agent_id, trace_id, dp);
        unimplemented!("TraceStore::append_decision_point")
    }

    /// Append an error to an in-progress trace.
    async fn append_error(
        &self,
        agent_id: &str,
        trace_id: &str,
        error: &TraceError,
    ) -> AmanResult<()> {
        let _ = (agent_id, trace_id, error);
        unimplemented!("TraceStore::append_error")
    }

    /// Load recent traces that contain at least one error, newest first.
    async fn load_recent_errors(
        &self,
        agent_id: &str,
        count: usize,
    ) -> AmanResult<Vec<TraceRecord>> {
        let _ = (agent_id, count);
        unimplemented!("TraceStore::load_recent_errors")
    }

    // ── Phase C: chain detection + tools ─────────────────────────────────

    /// Append a tool call record to an in-progress trace.
    async fn append_tool_call(
        &self,
        agent_id: &str,
        trace_id: &str,
        tc: &ToolCallRecord,
    ) -> AmanResult<()> {
        let _ = (agent_id, trace_id, tc);
        unimplemented!("TraceStore::append_tool_call")
    }

    /// Find traces whose outcome is `Partial` and that have no `ended_at_ms`.
    /// Used by Reflection step 1 (chain_tasks) to detect incomplete chains.
    async fn find_incomplete(&self, agent_id: &str) -> AmanResult<Vec<TraceRecord>> {
        let _ = agent_id;
        unimplemented!("TraceStore::find_incomplete")
    }

    /// Detect potential task chains by grouping related incomplete traces.
    /// Returns groups of traces that appear to be part of the same chain
    /// (same session, sequential task_type patterns).
    async fn detect_chains(
        &self,
        agent_id: &str,
    ) -> AmanResult<Vec<Vec<TraceRecord>>> {
        let _ = agent_id;
        unimplemented!("TraceStore::detect_chains")
    }

    // ── Phase D: management ──────────────────────────────────────────────

    /// Total trace count for the agent.
    async fn count(&self, agent_id: &str) -> AmanResult<u64> {
        let _ = agent_id;
        unimplemented!("TraceStore::count")
    }

    /// List all trace IDs for an agent, newest first.
    async fn list_all(&self, agent_id: &str) -> AmanResult<Vec<String>> {
        let _ = agent_id;
        unimplemented!("TraceStore::list_all")
    }

    /// Delete a specific trace by ID. Returns `true` if the trace existed.
    async fn delete_trace(&self, agent_id: &str, trace_id: &str) -> AmanResult<bool> {
        let _ = (agent_id, trace_id);
        unimplemented!("TraceStore::delete_trace")
    }

    /// Aggregate statistics across all traces for this agent.
    async fn stats_summary(&self, agent_id: &str) -> AmanResult<TraceStatsSummary> {
        let _ = agent_id;
        unimplemented!("TraceStore::stats_summary")
    }

    /// Delete traces older than `older_than_secs` seconds. Returns count pruned.
    async fn prune(&self, agent_id: &str, older_than_secs: u64) -> AmanResult<u64> {
        let _ = (agent_id, older_than_secs);
        unimplemented!("TraceStore::prune")
    }

    // ── Phase E: filtered queries ──────────────────────────────────────────

    /// Load traces matching a specific `task_type`, newest first.
    async fn load_by_task_type(
        &self,
        agent_id: &str,
        task_type: &str,
        limit: usize,
    ) -> AmanResult<Vec<TraceRecord>> {
        let _ = (agent_id, task_type, limit);
        unimplemented!("TraceStore::load_by_task_type")
    }

    /// Load traces whose `started_at_ms` falls within the given range
    /// (inclusive on both ends), newest first.
    async fn load_by_time_range(
        &self,
        agent_id: &str,
        start_ms: i64,
        end_ms: i64,
        limit: usize,
    ) -> AmanResult<Vec<TraceRecord>> {
        let _ = (agent_id, start_ms, end_ms, limit);
        unimplemented!("TraceStore::load_by_time_range")
    }
}
