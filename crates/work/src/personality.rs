// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! WorkPersonality — per-Agent work behaviour.
//!
//! Architecture ref: work-design.md v2 §3.4

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Serde helper: serialize Duration as f64 seconds.
pub(crate) mod serde_duration_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(d: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(d.as_secs_f64())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = f64::deserialize(deserializer)?;
        Ok(Duration::from_secs_f64(secs))
    }
}

// ---------------------------------------------------------------------------
// WorkPersonality
// ---------------------------------------------------------------------------

/// Defines how an Agent approaches work execution.
///
/// v2: simplified — no claim strategy, no task selection, no decomposition
/// tuning. External systems handle distribution; Agent just consumes its queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkPersonality {
    /// Whether the agent accepts work items from external sources.
    pub auto_claim: bool,

    /// Capability tags for matching work items.
    #[serde(default)]
    pub capabilities: Vec<String>,

    /// Maximum concurrent work items.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,

    /// Minimum interval between finishing one item and starting the next.
    #[serde(default = "default_work_cooldown", with = "serde_duration_secs")]
    pub work_cooldown: Duration,
}

fn default_max_concurrent() -> usize {
    1
}

fn default_work_cooldown() -> Duration {
    Duration::from_secs(5)
}

impl Default for WorkPersonality {
    fn default() -> Self {
        Self {
            auto_claim: true,
            capabilities: vec!["code".into(), "refactor".into(), "fix".into(), "review".into()],
            max_concurrent: 1,
            work_cooldown: Duration::from_secs(5),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_personality_is_valid() {
        let p = WorkPersonality::default();
        assert!(p.auto_claim);
        assert!(!p.capabilities.is_empty());
        assert_eq!(p.max_concurrent, 1);
        assert!(p.work_cooldown >= Duration::from_secs(5));
    }

    #[test]
    fn personality_serde_roundtrip() {
        let p = WorkPersonality::default();
        let json = serde_json::to_string(&p).expect("serialize");
        let deser: WorkPersonality = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p.auto_claim, deser.auto_claim);
        assert_eq!(p.capabilities, deser.capabilities);
    }
}
