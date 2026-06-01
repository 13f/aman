// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! StudyConfig — passive-queue configuration for the study system.
//!
//! Architecture ref: study-design.md v2 §9

use lifecycle::{ExecutionConfig, HooksConfig, QueueConfig, RetryConfig};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::types::StudyDepth;

mod duration_secs {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudyConfig {
    #[serde(default)]
    pub execution: ExecutionConfig,

    #[serde(default)]
    pub hooks: HooksConfig,

    #[serde(default)]
    pub queue: QueueConfig,

    #[serde(default)]
    pub retry: RetryConfig,

    /// 默认学习深度。
    #[serde(default)]
    pub default_depth: StudyDepth,

    /// 单阶段最大执行时间。
    #[serde(default = "default_phase_timeout", with = "duration_secs")]
    pub phase_timeout: Duration,

    /// 材料自动获取。
    #[serde(default)]
    pub materials: MaterialsConfig,

    /// 学习策略。
    #[serde(default)]
    pub learning: LearningConfig,

    /// 间隔重复。
    #[serde(default)]
    pub spaced_repetition: SpacedRepetitionConfig,

    /// 知识图谱。
    #[serde(default)]
    pub knowledge_graph: KnowledgeGraphConfig,
}

fn default_phase_timeout() -> Duration {
    Duration::from_secs(600)
}

impl Default for StudyConfig {
    fn default() -> Self {
        Self {
            execution: ExecutionConfig::default(),
            hooks: HooksConfig::default(),
            queue: QueueConfig::default(),
            retry: RetryConfig::default(),
            default_depth: StudyDepth::default(),
            phase_timeout: Duration::from_secs(600),
            materials: MaterialsConfig::default(),
            learning: LearningConfig::default(),
            spaced_repetition: SpacedRepetitionConfig::default(),
            knowledge_graph: KnowledgeGraphConfig::default(),
        }
    }
}

impl StudyConfig {
    pub fn validate(&self) -> Result<(), String> {
        self.execution.validate()?;
        self.queue.validate()?;
        self.retry.validate()?;
        if self.phase_timeout.is_zero() {
            return Err("study.phase_timeout must be > 0".into());
        }
        self.spaced_repetition.validate()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MaterialsConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaterialsConfig {
    #[serde(default = "default_true")]
    pub auto_gather: bool,
    #[serde(default)]
    pub search_sources: Vec<String>,
    #[serde(default = "default_max_candidates")]
    pub max_candidates: usize,
    #[serde(default = "default_min_relevance")]
    pub min_relevance: f64,
}

fn default_true() -> bool {
    true
}
fn default_max_candidates() -> usize {
    10
}
fn default_min_relevance() -> f64 {
    0.6
}

impl Default for MaterialsConfig {
    fn default() -> Self {
        Self {
            auto_gather: true,
            search_sources: vec!["arxiv".into(), "web_search".into(), "local_knowledge_graph".into()],
            max_candidates: 10,
            min_relevance: 0.6,
        }
    }
}

// ---------------------------------------------------------------------------
// LearningConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearningConfig {
    #[serde(default = "default_max_module_duration", with = "duration_secs")]
    pub max_module_duration: Duration,
    #[serde(default = "default_min_comprehension")]
    pub min_comprehension: f64,
    #[serde(default = "default_true")]
    pub auto_practice: bool,
}

fn default_max_module_duration() -> Duration {
    Duration::from_secs(600)
}
fn default_min_comprehension() -> f64 {
    0.7
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            max_module_duration: Duration::from_secs(600),
            min_comprehension: 0.7,
            auto_practice: true,
        }
    }
}

// ---------------------------------------------------------------------------
// SpacedRepetitionConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpacedRepetitionConfig {
    #[serde(default = "default_intervals")]
    pub intervals_days: Vec<u32>,
    #[serde(default = "default_max_review_rounds")]
    pub max_review_rounds: u32,
    #[serde(default = "default_ease_factor")]
    pub ease_factor: f64,
    #[serde(default = "default_min_interval")]
    pub min_interval_on_fail: u32,
}

fn default_intervals() -> Vec<u32> {
    vec![1, 3, 7, 14, 30, 60, 120]
}
fn default_max_review_rounds() -> u32 {
    7
}
fn default_ease_factor() -> f64 {
    2.5
}
fn default_min_interval() -> u32 {
    1
}

impl Default for SpacedRepetitionConfig {
    fn default() -> Self {
        Self {
            intervals_days: vec![1, 3, 7, 14, 30, 60, 120],
            max_review_rounds: 7,
            ease_factor: 2.5,
            min_interval_on_fail: 1,
        }
    }
}

impl SpacedRepetitionConfig {
    fn validate(&self) -> Result<(), String> {
        if self.ease_factor < 1.3 {
            return Err("spaced_repetition.ease_factor must be >= 1.3".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// KnowledgeGraphConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeGraphConfig {
    #[serde(default = "default_min_connections")]
    pub min_connections: usize,
    #[serde(default = "default_true")]
    pub auto_connect: bool,
    #[serde(default = "default_similarity")]
    pub similarity_threshold: f64,
}

fn default_min_connections() -> usize {
    2
}
fn default_similarity() -> f64 {
    0.6
}

impl Default for KnowledgeGraphConfig {
    fn default() -> Self {
        Self {
            min_connections: 2,
            auto_connect: true,
            similarity_threshold: 0.6,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = StudyConfig::default();
        assert!(config.validate().is_ok());
        assert_eq!(config.default_depth, StudyDepth::Read);
        assert_eq!(config.phase_timeout, Duration::from_secs(600));
    }

    #[test]
    fn zero_phase_timeout_is_invalid() {
        let mut config = StudyConfig::default();
        config.phase_timeout = Duration::ZERO;
        assert!(config.validate().is_err());
    }

    #[test]
    fn ease_factor_too_low_is_invalid() {
        let mut config = StudyConfig::default();
        config.spaced_repetition.ease_factor = 1.0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = StudyConfig::default();
        let json = serde_json::to_string(&config).expect("serialize");
        let deser: StudyConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(config.default_depth, deser.default_depth);
    }
}
