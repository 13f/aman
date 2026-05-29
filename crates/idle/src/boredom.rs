// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! BoredomActor — weighted random tag selection.
//!
//! When the agent has been in Boredom for `trigger_poll` consecutive polls,
//! a weighted random tag is selected from the configured activities. The
//! caller (manager → agent) decides how to route the tag to a system action.

use tracing::info;

use crate::types::BoredomConfig;

/// Picks a random activity tag based on boredom configuration.
pub struct BoredomActor {
    config: BoredomConfig,
}

impl BoredomActor {
    /// Create a new BoredomActor.
    #[must_use]
    pub fn new(config: BoredomConfig) -> Self {
        Self { config }
    }

    /// Try to pick a tag. Returns `Some(tag)` when:
    /// - `poll_count` == configured `trigger_poll`, AND
    /// - The weighted pick does NOT land on "idle"
    ///
    /// Returns `None` otherwise (wrong poll, idle picked, or empty config).
    pub fn pick(&self, poll_count: u32) -> Option<String> {
        if poll_count != self.config.trigger_poll {
            return None;
        }

        let tag = self.weighted_pick_tag()?;
        info!("random_hit:tag: {tag}");

        if tag == "idle" {
            return None;
        }

        Some(tag)
    }

    /// Weighted random tag selection. Weights are normalized internally.
    fn weighted_pick_tag(&self) -> Option<String> {
        let total: f64 = self.config.activities.iter().map(|a| a.weight).sum();
        if total <= 0.0 {
            return None;
        }

        let r: f64 = rand::random();
        let target = r * total;

        let mut acc = 0.0;
        for activity in &self.config.activities {
            acc += activity.weight;
            if target <= acc {
                return Some(activity.tag.clone());
            }
        }

        self.config.activities.last().map(|a| a.tag.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BoredomActivity;

    fn test_config() -> BoredomConfig {
        BoredomConfig {
            trigger_poll: 3,
            activities: vec![
                BoredomActivity { tag: "idle".into(), weight: 7.5 },
                BoredomActivity { tag: "work".into(), weight: 1.0 },
                BoredomActivity { tag: "internet".into(), weight: 1.5 },
            ],
        }
    }

    #[test]
    fn returns_none_when_poll_count_mismatch() {
        let actor = BoredomActor::new(test_config());
        assert!(actor.pick(1).is_none());
        assert!(actor.pick(2).is_none());
        assert!(actor.pick(4).is_none());
    }

    #[test]
    fn returns_some_on_trigger_poll() {
        // Only "work" has weight → guaranteed pick
        let config = BoredomConfig {
            trigger_poll: 3,
            activities: vec![BoredomActivity { tag: "work".into(), weight: 1.0 }],
        };
        let actor = BoredomActor::new(config);
        assert_eq!(actor.pick(3), Some("work".into()));
    }

    #[test]
    fn idle_tag_returns_none() {
        let config = BoredomConfig {
            trigger_poll: 3,
            activities: vec![BoredomActivity { tag: "idle".into(), weight: 1.0 }],
        };
        let actor = BoredomActor::new(config);
        assert!(actor.pick(3).is_none());
    }

    #[test]
    fn weighted_pick_respects_distribution() {
        let config = BoredomConfig {
            trigger_poll: 1,
            activities: vec![
                BoredomActivity { tag: "a".into(), weight: 0.0 },
                BoredomActivity { tag: "b".into(), weight: 1.0 },
            ],
        };
        let actor = BoredomActor::new(config);
        for _ in 0..100 {
            let tag = actor.weighted_pick_tag().expect("should pick");
            assert_eq!(tag, "b");
        }
    }
}
