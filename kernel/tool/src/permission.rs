#![forbid(unsafe_code)]

// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Permission review — structured tool sensitivity classification and
//! operator approval flow for sensitive tool execution.
//!
//! Complements hardline blocks (`security.rs`) with a softer gating
//! layer: tools are classified by sensitivity, and Medium/High tools
//! require operator confirmation.

use std::collections::HashMap;

/// Sensitivity classification for a tool.
///
/// Used by [`PermissionReviewer`] to decide whether operator approval
/// is needed before execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolSensitivity {
    /// Always allowed — harmless read-only tools (search, read, list).
    Low,
    /// Requires once-per-session approval (first use prompts user;
    /// subsequent identical calls skip the dialog).
    Medium,
    /// Requires per-call approval (every invocation needs confirmation).
    High,
}

/// Decision from the permission reviewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewDecision {
    /// Tool is safe to execute without further checks.
    AutoApproved,
    /// Operator approval is required before executing.
    RequiresApproval {
        tool_name: String,
        sensitivity: ToolSensitivity,
        reason: String,
    },
    /// Hard-denied (e.g., sandboxed context attempting High-sensitivity tool).
    Denied {
        reason: String,
    },
}

/// Session-scoped permission reviewer.
///
/// Caches approve/deny decisions per `(session_id, tool_name, args_hash)`
/// so that identical Medium-sensitivity calls within the same session
/// don't re-prompt.
#[derive(Debug, Clone)]
pub struct PermissionReviewer {
    /// Default sensitivity for tools not in the explicit map.
    default_sensitivity: ToolSensitivity,
    /// Explicit sensitivity overrides: tool_name → sensitivity.
    sensitivity_map: HashMap<String, ToolSensitivity>,
    /// Session-scoped cache: `(session_id, tool_name, args_hash)` → approved.
    session_cache: HashMap<(String, String, String), bool>,
}

impl Default for PermissionReviewer {
    fn default() -> Self {
        Self::new()
    }
}

impl PermissionReviewer {
    /// Create a new PermissionReviewer with built-in sensitivity defaults.
    #[must_use]
    pub fn new() -> Self {
        let mut sensitivity_map = HashMap::new();
        // ── Low (always-ok read tools) ──
        for name in &[
            "read", "list", "find", "grep", "web_search", "web_fetch",
        ] {
            sensitivity_map.insert((*name).to_string(), ToolSensitivity::Low);
        }
        // ── Medium (per-session cache) ──
        for name in &["write", "edit", "http", "file"] {
            sensitivity_map.insert((*name).to_string(), ToolSensitivity::Medium);
        }
        // ── High (per-call approval) ──
        for name in &["exec", "db"] {
            sensitivity_map.insert((*name).to_string(), ToolSensitivity::High);
        }

        Self {
            default_sensitivity: ToolSensitivity::Low,
            sensitivity_map,
            session_cache: HashMap::new(),
        }
    }

    /// Look up the sensitivity for a tool.
    #[must_use]
    pub fn sensitivity(&self, tool_name: &str) -> ToolSensitivity {
        self.sensitivity_map
            .get(tool_name)
            .copied()
            .unwrap_or(self.default_sensitivity)
    }

    /// Review a tool call and return a decision.
    ///
    /// `session_id` identifies the active session. `args_hash` is a
    /// content-based hash of the tool arguments (e.g., blake3 truncated
    /// to 16 hex chars).
    #[must_use]
    pub fn review(
        &self,
        session_id: &str,
        tool_name: &str,
        args_hash: &str,
    ) -> ReviewDecision {
        let sensitivity = self.sensitivity(tool_name);

        match sensitivity {
            ToolSensitivity::Low => ReviewDecision::AutoApproved,
            ToolSensitivity::Medium => {
                // Check session cache — if previously approved, skip dialog
                let cache_key = (
                    session_id.to_string(),
                    tool_name.to_string(),
                    args_hash.to_string(),
                );
                if self.session_cache.get(&cache_key).copied().unwrap_or(false) {
                    return ReviewDecision::AutoApproved;
                }
                ReviewDecision::RequiresApproval {
                    tool_name: tool_name.to_string(),
                    sensitivity,
                    reason: format!(
                        "Tool '{tool_name}' requires once-per-session approval. \
                         Subsequent identical calls in this session will be auto-approved."
                    ),
                }
            }
            ToolSensitivity::High => ReviewDecision::RequiresApproval {
                tool_name: tool_name.to_string(),
                sensitivity,
                reason: format!(
                    "Tool '{tool_name}' requires per-call approval. \
                     This operation can modify system state."
                ),
            },
        }
    }

    /// Record an approval decision in the session cache.
    pub fn record_decision(
        &mut self,
        session_id: &str,
        tool_name: &str,
        args_hash: &str,
        approved: bool,
    ) {
        let cache_key = (
            session_id.to_string(),
            tool_name.to_string(),
            args_hash.to_string(),
        );
        self.session_cache.insert(cache_key, approved);
    }

    /// Clear all cached decisions for a session (e.g., on session end).
    pub fn clear_session(&mut self, session_id: &str) {
        self.session_cache
            .retain(|(sid, _, _), _| sid != session_id);
    }

    /// Number of cached decisions.
    #[must_use]
    pub fn cache_size(&self) -> usize {
        self.session_cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_sensitivity_auto_approved() {
        let reviewer = PermissionReviewer::new();
        assert_eq!(
            reviewer.sensitivity("read"),
            ToolSensitivity::Low
        );
        assert_eq!(
            reviewer.review("s1", "read", "abc"),
            ReviewDecision::AutoApproved
        );
    }

    #[test]
    fn medium_sensitivity_requires_approval_first_time() {
        let reviewer = PermissionReviewer::new();
        assert_eq!(
            reviewer.sensitivity("write"),
            ToolSensitivity::Medium
        );
        let decision = reviewer.review("s1", "write", "hash1");
        assert!(
            matches!(decision, ReviewDecision::RequiresApproval { .. }),
            "first Medium call should require approval"
        );
    }

    #[test]
    fn medium_sensitivity_uses_cache_after_approval() {
        let mut reviewer = PermissionReviewer::new();
        reviewer.record_decision("s1", "write", "hash1", true);
        let decision = reviewer.review("s1", "write", "hash1");
        assert_eq!(decision, ReviewDecision::AutoApproved);
    }

    #[test]
    fn medium_sensitivity_different_args_still_requires() {
        let mut reviewer = PermissionReviewer::new();
        reviewer.record_decision("s1", "write", "hash1", true);
        let decision = reviewer.review("s1", "write", "hash2");
        assert!(
            matches!(decision, ReviewDecision::RequiresApproval { .. }),
            "different args should require new approval"
        );
    }

    #[test]
    fn high_sensitivity_always_requires_approval() {
        let mut reviewer = PermissionReviewer::new();
        assert_eq!(
            reviewer.sensitivity("exec"),
            ToolSensitivity::High
        );
        let decision = reviewer.review("s1", "exec", "hash1");
        assert!(matches!(decision, ReviewDecision::RequiresApproval { .. }));
        // Even after "approval", High tools still require per-call approval
        reviewer.record_decision("s1", "exec", "hash1", true);
        let decision2 = reviewer.review("s1", "exec", "hash1");
        assert!(
            matches!(decision2, ReviewDecision::RequiresApproval { .. }),
            "High-sensitivity tools should always require approval"
        );
    }

    #[test]
    fn unknown_tool_defaults_to_low() {
        let reviewer = PermissionReviewer::new();
        assert_eq!(
            reviewer.sensitivity("unknown_tool"),
            ToolSensitivity::Low
        );
    }

    #[test]
    fn clear_session_removes_cache() {
        let mut reviewer = PermissionReviewer::new();
        reviewer.record_decision("s1", "write", "h1", true);
        reviewer.record_decision("s2", "write", "h1", true);
        assert_eq!(reviewer.cache_size(), 2);
        reviewer.clear_session("s1");
        assert_eq!(reviewer.cache_size(), 1);
    }

    #[test]
    fn db_tool_is_high_sensitivity() {
        let reviewer = PermissionReviewer::new();
        assert_eq!(
            reviewer.sensitivity("db"),
            ToolSensitivity::High
        );
    }
}
