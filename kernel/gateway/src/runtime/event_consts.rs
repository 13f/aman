// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Centralized event-type and source string constants used across the
//! agent runtime.
//!
//! Before this module existed, magic strings like `"agent:harness"` (as
//! the event source) and `"tool:dispatched"` (as the event type) were
//! repeated 32+ times in `agent_harness.rs` alone. They were easy to
//! mistype, hard to rename, and impossible to audit for consistency.
//!
//! All event-type constants here are the exact strings passed to
//! `EventType::Custom(...)` or used as the `source` field on a published
//! `Event`. They MUST stay in lock-step with any subscribers.

#![forbid(unsafe_code)]

/// The `source` value stamped onto every event published by the agent
/// harness. Subscribers filter on this string.
pub const SOURCE_AGENT_HARNESS: &str = "agent:harness";

// ── Event types published by the agent harness ───────────────────────

/// A tool was dispatched for execution.
pub const EVT_TOOL_DISPATCHED: &str = "tool:dispatched";
/// A tool finished executing (success or failure).
pub const EVT_TOOL_COMPLETED: &str = "tool:completed";
/// A tool call was rejected by the security policy.
pub const EVT_TOOL_SECURITY_DENIED: &str = "tool:security_denied";
/// A skill invocation finished.
pub const EVT_SKILL_COMPLETED: &str = "skill:completed";

/// Agent has started processing a message and is no longer idle.
pub const EVT_AGENT_BUSY: &str = "agent:busy";
/// Agent finished processing and is idle.
pub const EVT_AGENT_IDLE: &str = "agent:idle";
/// The ReAct loop exceeded the maximum turn count.
#[allow(dead_code)]
pub const EVT_AGENT_MAX_TURNS_REACHED: &str = "agent:max_turns_reached";
/// The ReAct loop saw tool calls on this turn (gate for next iteration).
pub const EVT_AGENT_GOT_TOOL_CALLS: &str = "agent:got_tool_calls";
/// Tool results from the previous turn have been fed back to the LLM.
pub const EVT_AGENT_TOOL_RESULTS_FED_BACK: &str = "agent:tool_results_fed_back";
/// A direct-act invocation has started.
pub const EVT_AGENT_DIRECT_ACT_STARTED: &str = "agent:direct_act_started";
/// Auto-continue triggered.
pub const EVT_AGENT_AUTO_CONTINUE: &str = "agent:auto_continue";
/// Auto-continue stopped.
pub const EVT_AGENT_AUTO_CONTINUE_STOPPED: &str = "agent:auto_continue_stopped";
/// Conversation history was compressed to fit the token budget.
pub const EVT_AGENT_HISTORY_COMPRESSED: &str = "agent:history_compressed";
/// A reply was interrupted (e.g. via /stop).
pub const EVT_AGENT_REPLY_INTERRUPTED: &str = "agent:reply_interrupted";
/// A streaming reply was completed.
pub const EVT_AGENT_REPLY_READY: &str = "agent:reply_ready";
/// An error occurred during reply streaming.
pub const EVT_AGENT_REPLY_STREAM_ERROR: &str = "agent:reply_stream_error";
/// A detached process has been spawned and we're awaiting its completion.
pub const EVT_AGENT_AWAITING_DETACH: &str = "agent:awaiting_detach";
/// Token usage record emitted after an LLM call.
pub const EVT_AGENT_TOKEN_USED: &str = "agent:token_used";
/// Configuration warning emitted at startup.
pub const EVT_AGENT_CONFIG_WARNING: &str = "agent:config_warning";

/// LLM call started.
pub const EVT_LLM_CALL_STARTED: &str = "llm:call_started";
/// LLM call ended.
pub const EVT_LLM_CALL_ENDED: &str = "llm:call_ended";
/// LLM returned an error.
pub const EVT_LLM_ERROR: &str = "llm_error";

#[cfg(test)]
mod tests {
    use super::*;

    /// Guards against accidental renames: the constants must remain valid
    /// `EventType::Custom(...)` arguments (no embedded NULs, no empty strings).
    #[test]
    fn all_event_type_constants_are_non_empty() {
        let constants = [
            EVT_TOOL_DISPATCHED,
            EVT_TOOL_COMPLETED,
            EVT_TOOL_SECURITY_DENIED,
            EVT_SKILL_COMPLETED,
            EVT_AGENT_BUSY,
            EVT_AGENT_IDLE,
            EVT_AGENT_MAX_TURNS_REACHED,
            EVT_AGENT_GOT_TOOL_CALLS,
            EVT_AGENT_TOOL_RESULTS_FED_BACK,
            EVT_AGENT_DIRECT_ACT_STARTED,
            EVT_AGENT_AUTO_CONTINUE,
            EVT_AGENT_AUTO_CONTINUE_STOPPED,
            EVT_AGENT_HISTORY_COMPRESSED,
            EVT_AGENT_REPLY_INTERRUPTED,
            EVT_AGENT_REPLY_READY,
            EVT_AGENT_REPLY_STREAM_ERROR,
            EVT_AGENT_AWAITING_DETACH,
            EVT_AGENT_TOKEN_USED,
            EVT_AGENT_CONFIG_WARNING,
            EVT_LLM_CALL_STARTED,
            EVT_LLM_CALL_ENDED,
            EVT_LLM_ERROR,
        ];
        for c in constants {
            assert!(!c.is_empty(), "event-type constant must not be empty");
            assert!(!c.contains('\0'), "event-type constant must not contain NUL");
        }
    }

    #[test]
    fn source_constant_is_non_empty() {
        assert!(!SOURCE_AGENT_HARNESS.is_empty());
    }
}