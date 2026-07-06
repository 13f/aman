// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Experience translator — determines the agent's experience level for a task.
//!
//! The translator is **pure logic** (no LLM, no I/O). It takes evidence counts
//! and returns an Experience variant + behavioral flags. The LLM-based task_tag
//! matching happens elsewhere (in the gateway or cognitive-llm); the translator
//! only decides *what to do* once a tag match result is known.

use crate::ConfidenceLevel;

/// The agent's experience level for a given task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Experience {
    /// High-confidence experience exists — agent can skip investigation.
    Confident,
    /// EXP.md is empty — bootstrap mode: execute but extract experience eagerly.
    Bootstrap,
    /// No matching experience found — normal flow.
    Untouched,
    /// Negative experience exists — avoid the triggering tools/patterns.
    Apprehensive,
}

/// Behavioral flags output by the translator.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExperienceFlags {
    /// If true, CognitiveEngine should skip the scout phase.
    pub skip_scout: bool,
    /// If true, event subscriber should trigger experience extraction after completion.
    pub trigger_extraction: bool,
}

/// Evidence counts for a strategy — input to the translator.
#[derive(Debug, Clone, Copy)]
pub struct ExperienceEvidence {
    /// Historical success ratio (0.0–1.0).
    pub pattern_score: f64,
    /// Total times this strategy has been applied.
    pub uses: u32,
    /// Number of successful applications.
    pub successes: u32,
}

impl ExperienceEvidence {
    /// No evidence available (untouched).
    pub fn none() -> Self {
        Self {
            pattern_score: 0.0,
            uses: 0,
            successes: 0,
        }
    }
}

/// Thresholds for experience classification.
#[derive(Debug, Clone, Copy)]
pub struct ExperienceThresholds {
    /// pattern_score above this with min_uses evidence → Confident.
    pub confident_score: f64,
    /// Minimum uses required for confident.
    pub confident_min_uses: u32,
    /// pattern_score below this with min_uses evidence → Apprehensive.
    pub apprehensive_score: f64,
    /// Minimum uses required for apprehensive.
    pub apprehensive_min_uses: u32,
}

impl Default for ExperienceThresholds {
    fn default() -> Self {
        Self {
            confident_score: 0.7,
            confident_min_uses: 3,
            apprehensive_score: 0.3,
            apprehensive_min_uses: 2,
        }
    }
}

/// Determine experience level from evidence.
///
/// This is the core translator function — pure logic, no LLM, no I/O.
pub fn translate_experience(
    evidence: Option<ExperienceEvidence>,
    thresholds: ExperienceThresholds,
    exp_md_empty: bool,
) -> (Experience, ExperienceFlags) {
    // Bootstrap: EXP.md is empty
    if exp_md_empty {
        return (
            Experience::Bootstrap,
            ExperienceFlags {
                trigger_extraction: true,
                ..Default::default()
            },
        );
    }

    let Some(ev) = evidence else {
        return (Experience::Untouched, ExperienceFlags::default());
    };

    // Not enough evidence → Untouched (neutral)
    if ev.uses < thresholds.confident_min_uses && ev.uses < thresholds.apprehensive_min_uses
    {
        return (Experience::Untouched, ExperienceFlags::default());
    }

    // High confidence: pattern_score above threshold with enough evidence
    if ev.pattern_score >= thresholds.confident_score
        && ev.uses >= thresholds.confident_min_uses
    {
        return (
            Experience::Confident,
            ExperienceFlags {
                skip_scout: true,
                ..Default::default()
            },
        );
    }

    // Apprehensive: pattern_score below threshold with enough evidence
    if ev.pattern_score <= thresholds.apprehensive_score
        && ev.uses >= thresholds.apprehensive_min_uses
    {
        return (
            Experience::Apprehensive,
            ExperienceFlags::default(),
        );
    }

    // Between thresholds → Untouched
    (Experience::Untouched, ExperienceFlags::default())
}

/// Map Experience to a ConfidenceLevel for the Decision.
pub fn experience_to_confidence(experience: Experience) -> ConfidenceLevel {
    match experience {
        Experience::Confident => ConfidenceLevel::Normal,
        Experience::Bootstrap => ConfidenceLevel::Normal,
        Experience::Untouched => ConfidenceLevel::Normal,
        Experience::Apprehensive => ConfidenceLevel::Low,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bootstrap_when_empty() {
        let (exp, flags) = translate_experience(None, ExperienceThresholds::default(), true);
        assert_eq!(exp, Experience::Bootstrap);
        assert!(flags.trigger_extraction);
        assert!(!flags.skip_scout);
    }

    #[test]
    fn test_confident() {
        let ev = ExperienceEvidence {
            pattern_score: 0.9,
            uses: 10,
            successes: 9,
        };
        let (exp, flags) = translate_experience(Some(ev), ExperienceThresholds::default(), false);
        assert_eq!(exp, Experience::Confident);
        assert!(flags.skip_scout);
    }

    #[test]
    fn test_apprehensive() {
        let ev = ExperienceEvidence {
            pattern_score: 0.2,
            uses: 5,
            successes: 1,
        };
        let (exp, _flags) = translate_experience(Some(ev), ExperienceThresholds::default(), false);
        assert_eq!(exp, Experience::Apprehensive);
    }

    #[test]
    fn test_untouched_no_evidence() {
        let (exp, _flags) = translate_experience(None, ExperienceThresholds::default(), false);
        assert_eq!(exp, Experience::Untouched);
    }

    #[test]
    fn test_untouched_low_uses() {
        let ev = ExperienceEvidence {
            pattern_score: 0.5,
            uses: 1,
            successes: 0,
        };
        let (exp, _flags) = translate_experience(Some(ev), ExperienceThresholds::default(), false);
        assert_eq!(exp, Experience::Untouched);
    }

    #[test]
    fn test_confidence_mapping() {
        assert_eq!(
            experience_to_confidence(Experience::Confident),
            ConfidenceLevel::Normal
        );
        assert_eq!(
            experience_to_confidence(Experience::Apprehensive),
            ConfidenceLevel::Low
        );
    }
}
