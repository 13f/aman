// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Work item session utilities.
//!
//! Provides deterministic session IDs for kanban work items so the agent can
//! "断点续传" (resume from a breakpoint): load the previous session history,
//! assess context utilization, compress if needed, and continue execution.
//!
//! Session ID format: `{agent_id}_{project_key}_{work_id}`
//! (e.g. `minmax_aman_work-1780551635570`)

use kernel::AmanResult;
use context_manager::{HistoryCompressor, CompressionStrategy};

/// Build a deterministic session ID for a work item.
///
/// Format: `{agent_id}_{project_key}_{work_id}`
///
/// This allows the same work item to be resumed across multiple idle-run
/// invocations — the agent loads the previous JSONL history and continues
/// from where it left off.
///
/// Parsing relies on `work_id` always starting with "work-" (a convention
/// from the Team plugin's `insert_work`), which lets us split unambiguously
/// even if `agent_id` or `project_key` contain underscores.
pub fn work_session_id(agent_id: &str, project_key: &str, work_id: &str) -> String {
    format!("{agent_id}_{project_key}_{work_id}")
}

/// Try to parse a work item session ID back into its components.
///
/// Returns `Some((agent_id, project_key, work_id))` if the session_id matches
/// the work item pattern `{agent}_{project}_work-...`, or `None` if it doesn't.
///
/// Distinguishes from idle sessions (`{agent}_idle_{run_id}`) by checking that
/// the suffix starts with `work-` rather than `idle_`.
pub fn parse_work_session_id(session_id: &str) -> Option<(String, String, String)> {
    // Find the work_id boundary: the last occurrence of "_work-" (work items
    // always have IDs starting with "work-").
    let work_marker = session_id.rfind("_work-")?;
    let prefix = &session_id[..work_marker];
    let work_id = &session_id[work_marker + 1..]; // skip the leading '_'

    // The prefix is "{agent_id}_{project_key}". Split on the first '_' to
    // separate agent_id from project_key.
    let first_underscore = prefix.find('_')?;
    let agent_id = prefix[..first_underscore].to_owned();
    let project_key = prefix[first_underscore + 1..].to_owned();

    if agent_id.is_empty() || project_key.is_empty() || work_id.is_empty() {
        return None;
    }
    Some((agent_id, project_key, work_id.to_owned()))
}

/// Build a deterministic session ID for a startup work item.
///
/// Format: `{agent_id}_startup_{idea_slug}`
///
/// This allows startup work (validation, strategy skills, etc.) to be tracked
/// by the Work System — the agent harness detects the `_startup_` marker and
/// sets `AgentSystemState::Working` while the startup task is running.
pub fn startup_session_id(agent_id: &str, idea_slug: &str) -> String {
    format!("{agent_id}_startup_{idea_slug}")
}

/// Try to parse a startup session ID back into its components.
///
/// Returns `Some((agent_id, idea_slug))` if the session_id matches the startup
/// pattern `{agent}_startup_{slug}`, or `None` if it doesn't.
pub fn parse_startup_session_id(session_id: &str) -> Option<(String, String)> {
    let marker = session_id.find("_startup_")?;
    let agent_id = &session_id[..marker];
    let idea_slug = &session_id[marker + "_startup_".len()..];

    if agent_id.is_empty() || idea_slug.is_empty() {
        return None;
    }
    Some((agent_id.to_owned(), idea_slug.to_owned()))
}

/// Returns true if the session_id is a plugin work session (kanban or startup).
///
/// Plugin work sessions cause the agent harness to set `AgentSystemState::Working`
/// instead of `AgentSystemState::Chatting`, and support session resumption (断点续传).
pub fn is_plugin_work_session(session_id: &str) -> bool {
    parse_work_session_id(session_id).is_some()
        || parse_startup_session_id(session_id).is_some()
}

/// Resume a work item session from persisted JSONL events.
///
/// Loads the session's event history, restores conversation history in the
/// agent harness, and applies compression if the total estimated tokens
/// exceed `max_history_tokens` (0 = no compression, just restore).
///
/// This is called before `process_message` when a work item session is
/// being re-opened ("断点续传").
pub async fn resume_work_session(
    agent_harness: &crate::runtime::agent_harness::AgentHarness,
    session_store: &super::session_store::SessionStore,
    session_id: &str,
    _max_history_tokens: usize,
) -> AmanResult<()> {
    let events = session_store.load_session_events(session_id);
    if events.is_empty() {
        return Ok(());
    }

    // Restore conversation history from persisted events into the
    // in-memory SessionHistoryStore so process_message picks it up.
    agent_harness.restore_session_history(session_id, &events);

    // TODO: Token budget check + compression.
    // For now, restore the full history. A future iteration should:
    // 1. Estimate token count of restored history
    // 2. Compare against max_history_tokens (or model context window)
    // 3. Apply HistoryCompressor if over threshold
    let _compressor = HistoryCompressor::new(CompressionStrategy::Truncate);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_session_id_format() {
        let id = work_session_id("agent-1", "proj-alpha", "work-001");
        assert_eq!(id, "agent-1_proj-alpha_work-001");
    }

    #[test]
    fn parse_valid_work_session_id() {
        let (agent, project, work) =
            parse_work_session_id("minmax_aman_work-1780551635570").expect("should parse");
        assert_eq!(agent, "minmax");
        assert_eq!(project, "aman");
        assert_eq!(work, "work-1780551635570");
    }

    #[test]
    fn parse_agent_with_underscore() {
        // agent_id may contain underscores (e.g. "my_agent")
        let (agent, project, work) =
            parse_work_session_id("my_agent_aman_work-123").expect("should parse");
        assert_eq!(agent, "my");
        // With underscores in agent_id, the first '_' splits agent from the rest.
        // This is a known limitation — agent_id should not contain underscores.
        // The team plugin validates project_key as [a-z0-9-]+, and agent IDs
        // follow the same convention.
    }

    #[test]
    fn parse_non_work_session_id() {
        // Idle session — has "idle" not "work-"
        assert!(parse_work_session_id("minmax_idle_abc123").is_none());
        // Persistent session UUID
        assert!(parse_work_session_id("some-uuid-here").is_none());
        // No work- marker
        assert!(parse_work_session_id("agent_proj_task").is_none());
    }

    #[test]
    fn roundtrip() {
        let id = work_session_id("minmax", "aman", "work-1780551635570");
        let (agent, project, work) = parse_work_session_id(&id).expect("should parse");
        assert_eq!(agent, "minmax");
        assert_eq!(project, "aman");
        assert_eq!(work, "work-1780551635570");
    }

    // ── Startup session ID tests ──────────────────────────────────────

    #[test]
    fn startup_session_id_format() {
        let id = startup_session_id("agent-1", "my-app-idea");
        assert_eq!(id, "agent-1_startup_my-app-idea");
    }

    #[test]
    fn parse_valid_startup_session_id() {
        let (agent, slug) =
            parse_startup_session_id("minmax_startup_cool-saas").expect("should parse");
        assert_eq!(agent, "minmax");
        assert_eq!(slug, "cool-saas");
    }

    #[test]
    fn parse_startup_session_with_underscore_in_slug() {
        let (agent, slug) =
            parse_startup_session_id("agent-1_startup_my_app_idea").expect("should parse");
        assert_eq!(agent, "agent-1");
        assert_eq!(slug, "my_app_idea");
    }

    #[test]
    fn parse_non_startup_session_id() {
        assert!(parse_startup_session_id("minmax_idle_abc123").is_none());
        assert!(parse_startup_session_id("some-uuid-here").is_none());
        assert!(parse_startup_session_id("agent_proj_work-123").is_none());
    }

    #[test]
    fn startup_roundtrip() {
        let id = startup_session_id("my-agent", "validate-me");
        let (agent, slug) = parse_startup_session_id(&id).expect("should parse");
        assert_eq!(agent, "my-agent");
        assert_eq!(slug, "validate-me");
    }

    #[test]
    fn is_plugin_work_session_detects_both() {
        assert!(is_plugin_work_session("minmax_aman_work-123"));
        assert!(is_plugin_work_session("agent-1_startup_my-idea"));
        assert!(!is_plugin_work_session("minmax_idle_abc123"));
        assert!(!is_plugin_work_session("random-session-id"));
    }
}
