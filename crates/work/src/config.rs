// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! WorkConfig — YAML configuration for the Work System.
//!
//! Architecture ref: work-design.md v2 §6

use serde::{Deserialize, Serialize};

use crate::personality::WorkPersonality;

// ---------------------------------------------------------------------------
// WorkConfig
// ---------------------------------------------------------------------------

/// Top-level work system configuration.
///
/// v2: simplified — no board connection, no review config.
/// External systems push work items; the Work System is a passive consumer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct WorkConfig {
    /// Agent work personality.
    #[serde(default)]
    pub personality: WorkPersonality,
}

impl WorkConfig {
    /// Validate the configuration.
    pub fn validate(&self) -> Result<(), String> {
        if self.personality.max_concurrent == 0 {
            return Err("work.personality.max_concurrent must be >= 1".into());
        }
        if self.personality.auto_claim && self.personality.capabilities.is_empty() {
            return Err(
                "work.personality.capabilities must not be empty when auto_claim=true".into()
            );
        }
        Ok(())
    }
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
}
