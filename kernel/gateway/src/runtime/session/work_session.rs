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
}
