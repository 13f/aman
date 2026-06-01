// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! SystemSpec trait — the glue between the generic LifecycleEngine and a
//! specific life-domain system (work, study, daily-life).

use std::future::Future;

use serde::Serialize;

use crate::types::{IdleSignal, LifecycleError, StepOutput};

/// SystemSpec provides all the type-level and behavioural glue between the
/// generic [`LifecycleEngine`](super::engine::LifecycleEngine) and a specific
/// life-domain system (work, study, daily-life).
///
/// Each system implements this trait on its own config/state struct, which
/// the engine holds and delegates to.
pub trait SystemSpec: Send + Sync + 'static {
    /// The item type pushed to this system's queue.
    type Item: Clone + Send + Sync + Serialize + 'static;
    /// The step type for internal execution chaining.
    type Step: Clone + Send + Sync + 'static;

    // ── Routing constants ──────────────────────────────────────────

    /// Event source string for bus routing (e.g. "work.system").
    fn event_source() -> &'static str;
    /// Event kind for internal step-execution events (e.g. "work.step.execute").
    fn step_event_kind() -> &'static str;
    /// Event kind for WorkItemAssigned-style events for routing.
    fn assigned_kind() -> &'static str;
    /// Event kind for WorkItemCompleted-style events for routing.
    fn completed_kind() -> &'static str;
    /// Event kind for WorkItemFailed-style events for routing.
    fn failed_kind() -> &'static str;
    /// Event kind for Interrupt events for routing.
    fn interrupt_kind() -> &'static str;

    // ── Item accessors ─────────────────────────────────────────────

    fn item_id(item: &Self::Item) -> String;
    fn notify_on_complete(item: &Self::Item) -> bool;

    // ── Serialization — event payloads ─────────────────────────────

    /// Serialize the item as a JSON value.
    fn serialize_item(item: &Self::Item) -> serde_json::Value;
    /// Build the full Assigned event payload (for bus publishing).
    fn make_assigned_payload(item: &Self::Item, source: serde_json::Value) -> serde_json::Value;
    /// Build the full Completed event payload.
    fn make_completed_payload(
        item_id: &str,
        result: serde_json::Value,
        duration_secs: f64,
    ) -> serde_json::Value;
    /// Build the full Failed event payload.
    fn make_failed_payload(
        item_id: &str,
        error: &LifecycleError,
        retryable: bool,
    ) -> serde_json::Value;
    /// Build the internal StepEvent payload.
    fn make_step_payload(step_index: usize) -> serde_json::Value;
    /// Build the global-bus result notification.
    fn make_result_notify(item_id: &str, result: &serde_json::Value, agent_id: &str) -> serde_json::Value;
    /// Build the global-bus failure notification.
    fn make_failure_notify(item_id: &str, error_msg: &str, agent_id: &str) -> serde_json::Value;

    // ── Step execution (domain-specific) ───────────────────────────

    /// Produce a default single step when auto_decompose is disabled and no
    /// predefined steps exist.
    fn default_step(item: &Self::Item, max_retries: u32) -> Self::Step;

    /// Return the max retries for a step (used by the engine to decide retry).
    fn step_max_retries(step: &Self::Step) -> u32;

    /// Decompose an item into execution steps. Called when the item has no
    /// predefined steps and auto_decompose is enabled.
    fn decompose(
        &self,
        item: &Self::Item,
        max_retries: u32,
    ) -> impl Future<Output = Vec<Self::Step>> + Send;

    /// Execute a single step and return its output.
    fn execute_step_impl(
        &self,
        item: &Self::Item,
        step: &Self::Step,
        step_index: usize,
    ) -> impl Future<Output = Result<StepOutput, LifecycleError>> + Send;

    // ── Result collection ──────────────────────────────────────────

    /// Collect step outputs into a result value (serialized for event payload).
    fn collect_result(item: &Self::Item, outputs: &[StepOutput]) -> serde_json::Value;

    // ── Idle signal ────────────────────────────────────────────────

    fn completion_signal(item: &Self::Item) -> IdleSignal;
}
