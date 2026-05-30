// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Work item session utilities.
//!
//! Provides deterministic session IDs for kanban work items so the agent can
//! "断点续传" (resume from a breakpoint): load the previous session history,
//! assess context utilization, compress if needed, and continue execution.
//!
//! Session ID format: `{agent_id}:work:{project_key}:{work_id}`

use kernel::AmanResult;
use crate::runtime::history_compressor::{HistoryCompressor, CompressionStrategy};

/// Build a deterministic session ID for a work item.
///
/// Format: `{agent_id}:work:{project_key}:{work_id}`
///
/// This allows the same work item to be resumed across multiple idle-run
/// invocations — the agent loads the previous JSONL history and continues
/// from where it left off.
pub fn work_session_id(agent_id: &str, project_key: &str, work_id: &str) -> String {
    format!("{agent_id}:work:{project_key}:{work_id}")
}

/// Try to parse a work item session ID back into its components.
///
/// Returns `Some((agent_id, project_key, work_id))` if the session_id matches
/// the work item pattern, or `None` if it doesn't.
pub fn parse_work_session_id(session_id: &str) -> Option<(String, String, String)> {
    let parts: Vec<&str> = session_id.splitn(4, ':').collect();
    if parts.len() == 4 && parts[1] == "work" {
        Some((
            parts[0].to_owned(),
            parts[2].to_owned(),
            parts[3].to_owned(),
        ))
    } else {
        None
    }
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
        assert_eq!(id, "agent-1:work:proj-alpha:work-001");
    }

    #[test]
    fn parse_valid_work_session_id() {
        let (agent, project, work) =
            parse_work_session_id("aman:work:myproject:work-123").expect("should parse");
        assert_eq!(agent, "aman");
        assert_eq!(project, "myproject");
        assert_eq!(work, "work-123");
    }

    #[test]
    fn parse_non_work_session_id() {
        // Regular session — not a work item session
        assert!(parse_work_session_id("aman:idle:abc123").is_none());
        // Persistent session UUID
        assert!(parse_work_session_id("some-uuid-here").is_none());
        // Too few segments
        assert!(parse_work_session_id("agent:work:proj").is_none());
    }

    #[test]
    fn parse_work_id_with_colons() {
        // work_id itself might contain separators — only split on first 3 ':'
        let (agent, project, work) =
            parse_work_session_id("bot:work:proj:work-2026-05-30T12:00:00Z")
                .expect("should parse");
        assert_eq!(agent, "bot");
        assert_eq!(project, "proj");
        // The rest after the 3rd ':' is the work_id
        assert_eq!(work, "work-2026-05-30T12:00:00Z");
    }
}
