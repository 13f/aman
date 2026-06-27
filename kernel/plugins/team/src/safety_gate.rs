// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Safety gate handler — dangerous action interception + confidence threshold.
//!
//! Architecture ref: docs/team-architect.md §9

use crate::config::SafetyGateConfig;
use crate::store::{SafetyGateReason, TeamStore};
use regex::Regex;
use tracing::warn;

/// Result of a safety gate check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyResult {
    /// Action is allowed.
    Allowed,
    /// Action is blocked — requires human decision.
    Blocked {
        reason: String,
        requires_human: bool,
    },
    /// Agent confidence is too low — pending human review.
    PendingHumanDecision,
}

/// Handler that evaluates actions and confidence against safety gate rules.
pub struct SafetyGateHandler {
    config: SafetyGateConfig,
    store: TeamStore,
    /// Compiled regex patterns from config.dangerous_actions.
    patterns: Vec<(Regex, bool)>,
}

impl SafetyGateHandler {
    /// Create a new handler, compiling all dangerous-action patterns.
    pub fn new(config: SafetyGateConfig, store: TeamStore) -> Result<Self, String> {
        let mut patterns = Vec::new();
        for da in &config.dangerous_actions {
            let re = Regex::new(&da.pattern)
                .map_err(|e| format!("invalid pattern '{}': {e}", da.pattern))?;
            patterns.push((re, da.require_human));
        }
        Ok(Self {
            config,
            store,
            patterns,
        })
    }

    /// Check whether an action is safe to execute.
    ///
    /// Returns `SafetyResult::Allowed` if no dangerous pattern matches.
    /// Returns `SafetyResult::Blocked` with the reason if a pattern matches.
    pub fn check_action(
        &self,
        action: &str,
        agent_id: &str,
        work_item_id: &str,
    ) -> SafetyResult {
        for (re, require_human) in &self.patterns {
            if re.is_match(action) {
                let reason = format!("dangerous action: pattern '{}' matched", re.as_str());
                warn!(
                    agent_id,
                    work_item_id,
                    action,
                    pattern = %re.as_str(),
                    "SafetyGate: dangerous action blocked"
                );
                // Log to store (best-effort — don't fail the check on store error)
                let _ = self.store.insert_safety_log(
                    work_item_id,
                    agent_id,
                    action,
                    SafetyGateReason::DangerousAction,
                );
                return SafetyResult::Blocked {
                    reason,
                    requires_human: *require_human,
                };
            }
        }
        SafetyResult::Allowed
    }

    /// Check whether the agent's confidence meets the minimum threshold.
    ///
    /// Returns `SafetyResult::PendingHumanDecision` if confidence is too low.
    pub fn check_confidence(
        &self,
        confidence: f64,
        work_item_id: &str,
        agent_id: &str,
    ) -> SafetyResult {
        if confidence < self.config.min_confidence {
            warn!(
                agent_id,
                work_item_id,
                confidence,
                min_confidence = self.config.min_confidence,
                "SafetyGate: low confidence — pending human decision"
            );
            let _ = self.store.insert_safety_log(
                work_item_id,
                agent_id,
                "",
                SafetyGateReason::LowConfidence,
            );
            return SafetyResult::PendingHumanDecision;
        }
        SafetyResult::Allowed
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DangerousActionPattern, SafetyGateConfig};
    use tempfile::tempdir;

    fn handler_with_patterns(patterns: &[(&str, bool)]) -> SafetyGateHandler {
        let config = SafetyGateConfig {
            dangerous_actions: patterns
                .iter()
                .map(|(p, h)| DangerousActionPattern {
                    pattern: p.to_string(),
                    require_human: *h,
                })
                .collect(),
            min_confidence: 0.7,
            max_autonomous_actions_without_human: 20,
        };
        let dir = tempdir().unwrap();
        let store = TeamStore::open(&dir.path().join("test.db")).unwrap();
        SafetyGateHandler::new(config, store).unwrap()
    }

    #[test]
    fn allow_safe_action() {
        let handler = handler_with_patterns(&[("rm -rf", true)]);
        let result = handler.check_action("cargo build", "coder", "task-1");
        assert_eq!(result, SafetyResult::Allowed);
    }

    #[test]
    fn block_dangerous_action() {
        let handler = handler_with_patterns(&[("rm -rf", true), ("git push --force", true)]);
        let result = handler.check_action("I will rm -rf /tmp/build", "coder", "task-2");
        assert!(matches!(result, SafetyResult::Blocked { .. }));
    }

    #[test]
    fn regex_pattern_matches_mid_string() {
        let handler = handler_with_patterns(&[("DROP |DELETE FROM|TRUNCATE", true)]);
        let result = handler.check_action("Execute: DROP TABLE users CASCADE", "coder", "task-3");
        assert!(matches!(result, SafetyResult::Blocked { .. }));
    }

    #[test]
    fn confidence_below_threshold() {
        let handler = handler_with_patterns(&[]);
        let result = handler.check_confidence(0.5, "task-4", "coder");
        assert_eq!(result, SafetyResult::PendingHumanDecision);
    }

    #[test]
    fn confidence_above_threshold() {
        let handler = handler_with_patterns(&[]);
        let result = handler.check_confidence(0.85, "task-5", "coder");
        assert_eq!(result, SafetyResult::Allowed);
    }
}
