// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Data structures for EXP.md entries.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The kind of experience entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperienceKind {
    /// A tool combination or strategy that works.
    ToolStrategy,
    /// A judgment pattern for decision-making.
    JudgmentPattern,
    /// An anti-pattern — something to avoid.
    AntiPattern,
    /// A gotcha — a的具体陷阱和绕过方式.
    Gotcha,
}

impl fmt::Display for ExperienceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ToolStrategy => write!(f, "Tool Strategies"),
            Self::JudgmentPattern => write!(f, "Judgment Patterns"),
            Self::AntiPattern => write!(f, "Anti-Patterns"),
            Self::Gotcha => write!(f, "Gotchas"),
        }
    }
}

/// A tag that categorizes the experience by task type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExperienceTag(String);

impl ExperienceTag {
    pub fn new(tag: impl Into<String>) -> Self {
        Self(tag.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExperienceTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}]", self.0)
    }
}

impl From<String> for ExperienceTag {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ExperienceTag {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// A single experience entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperienceEntry {
    /// The experience category (which section this belongs to).
    pub category: ExperienceKind,
    /// The task tag.
    pub tag: ExperienceTag,
    /// Short human-readable description.
    pub description: String,
    /// The strategy/pattern/anti-pattern/gotcha content.
    pub content: String,
    /// Confidence score 0.0–1.0 (成功率).
    #[serde(default)]
    pub confidence: f64,
    /// Total times this experience has been applied.
    #[serde(default)]
    pub uses: u32,
    /// Number of successful applications.
    #[serde(default)]
    pub successes: u32,
    /// Whether this entry needs verification (stale check).
    #[serde(default)]
    pub needs_verification: bool,
    /// Session references that contributed to this entry.
    #[serde(default)]
    pub learned_from: Vec<String>,
}

impl ExperienceEntry {
    /// Calculate pattern score (success ratio).
    pub fn pattern_score(&self) -> f64 {
        if self.uses == 0 {
            0.5 // neutral default
        } else {
            self.successes as f64 / self.uses as f64
        }
    }

    /// Whether this entry has enough evidence to be actionable.
    pub fn has_evidence(&self, min_uses: u32) -> bool {
        self.uses >= min_uses
    }
}

/// The full EXP.md structure — a collection of experience entries grouped by kind.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExpMd {
    pub strategies: Vec<ExperienceEntry>,
    pub patterns: Vec<ExperienceEntry>,
    pub anti_patterns: Vec<ExperienceEntry>,
    pub gotchas: Vec<ExperienceEntry>,
}

impl ExpMd {
    /// Create an empty EXP.md.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Check if EXP.md has any entries.
    pub fn is_empty(&self) -> bool {
        self.strategies.is_empty()
            && self.patterns.is_empty()
            && self.anti_patterns.is_empty()
            && self.gotchas.is_empty()
    }

    /// Find a strategy by tag.
    pub fn find_strategy(&self, tag: &ExperienceTag) -> Option<&ExperienceEntry> {
        self.strategies.iter().find(|e| e.tag == *tag)
    }

    /// Find any entry across all categories matching the predicate.
    pub fn find_entry<P>(&self, pred: P) -> Option<&ExperienceEntry>
    where
        P: Fn(&ExperienceEntry) -> bool,
    {
        self.strategies
            .iter()
            .chain(&self.patterns)
            .chain(&self.anti_patterns)
            .chain(&self.gotchas)
            .find(|e| pred(e))
    }
}

/// Evidence counts for a strategy — used by the translator to determine Experience level.
#[derive(Debug, Clone, Copy)]
pub struct StrategyEvidence {
    pub pattern_score: f64,
    pub uses: u32,
    pub successes: u32,
}
