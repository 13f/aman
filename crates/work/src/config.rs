// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! WorkConfig — YAML configuration for the Work System.
//!
//! Architecture ref: work-design.md §6

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::personality::{serde_duration_secs, WorkPersonality};

// ---------------------------------------------------------------------------
// §6.1 WorkConfig
// ---------------------------------------------------------------------------

/// Top-level work system configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkConfig {
    /// Agent work personality.
    #[serde(default)]
    pub personality: WorkPersonality,

    /// Task board connection settings.
    #[serde(default)]
    pub board: BoardConfig,

    /// Review settings.
    #[serde(default)]
    pub review: ReviewConfig,
}

impl Default for WorkConfig {
    fn default() -> Self {
        Self {
            personality: WorkPersonality::default(),
            board: BoardConfig::default(),
            review: ReviewConfig::default(),
        }
    }
}

impl WorkConfig {
    /// Validate the configuration.
    ///
    /// Architecture ref: work-design.md §6.2
    pub fn validate(&self) -> Result<(), String> {
        if self.personality.max_concurrent == 0 {
            return Err("work.personality.max_concurrent must be >= 1".into());
        }
        if self.personality.work_cooldown < Duration::from_secs(5) {
            return Err(
                "work.personality.work_cooldown must be >= 5s to avoid busy-loop".into()
            );
        }
        if self.personality.auto_claim && self.personality.capabilities.is_empty() {
            return Err(
                "work.personality.capabilities must not be empty when auto_claim=true".into()
            );
        }
        if self.review.timeout < Duration::from_secs(1) {
            return Err("work.review.timeout must be >= 1s".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// BoardConfig
// ---------------------------------------------------------------------------

/// Connection settings for the task board.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardConfig {
    /// Board type: "kanban" | "team" | "custom"
    #[serde(default = "default_board_type")]
    pub board_type: String,

    /// Poll interval for TaskBoardUpdated events.
    #[serde(default = "default_poll_interval", with = "serde_duration_secs")]
    pub poll_interval: Duration,

    /// Query filter.
    #[serde(default)]
    pub query: BoardQuery,
}

impl Default for BoardConfig {
    fn default() -> Self {
        Self {
            board_type: default_board_type(),
            poll_interval: default_poll_interval(),
            query: BoardQuery::default(),
        }
    }
}

fn default_board_type() -> String {
    "kanban".into()
}

fn default_poll_interval() -> Duration {
    Duration::from_secs(30)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardQuery {
    /// Stages to pull tasks from.
    #[serde(default = "default_stages")]
    pub stages: Vec<String>,

    /// Max tasks to fetch per query.
    #[serde(default = "default_limit")]
    pub limit: usize,
}

impl Default for BoardQuery {
    fn default() -> Self {
        Self {
            stages: default_stages(),
            limit: default_limit(),
        }
    }
}

fn default_stages() -> Vec<String> {
    vec!["backlog".into(), "wip".into()]
}

fn default_limit() -> usize {
    20
}

// ---------------------------------------------------------------------------
// ReviewConfig
// ---------------------------------------------------------------------------

/// Review/verification settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewConfig {
    /// Whether to auto-verify results.
    #[serde(default = "default_true")]
    pub auto_verify: bool,

    /// Operations that require human approval.
    #[serde(default = "default_dangerous_ops")]
    pub require_human_approval_for: Vec<String>,

    /// Review timeout.
    #[serde(default = "default_review_timeout", with = "serde_duration_secs")]
    pub timeout: Duration,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            auto_verify: true,
            require_human_approval_for: default_dangerous_ops(),
            timeout: default_review_timeout(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_dangerous_ops() -> Vec<String> {
    vec![
        "git push --force".into(),
        "rm -rf".into(),
        "DROP TABLE".into(),
    ]
}

fn default_review_timeout() -> Duration {
    Duration::from_secs(120)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = WorkConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn zero_max_concurrent_is_invalid() {
        let mut config = WorkConfig::default();
        config.personality.max_concurrent = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn too_short_cooldown_is_invalid() {
        let mut config = WorkConfig::default();
        config.personality.work_cooldown = Duration::from_secs(2);
        assert!(config.validate().is_err());
    }

    #[test]
    fn empty_capabilities_with_auto_claim_is_invalid() {
        let mut config = WorkConfig::default();
        config.personality.auto_claim = true;
        config.personality.capabilities.clear();
        assert!(config.validate().is_err());
    }

    #[test]
    fn empty_capabilities_no_auto_claim_is_valid() {
        let mut config = WorkConfig::default();
        config.personality.auto_claim = false;
        config.personality.capabilities.clear();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn review_config_default_timeout() {
        let config = ReviewConfig::default();
        assert_eq!(config.timeout, Duration::from_secs(120));
        assert!(config.auto_verify);
        assert!(!config.require_human_approval_for.is_empty());
    }
}
