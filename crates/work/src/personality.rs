// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! WorkPersonality — 每个 Agent 的工作人格。
//!
//! Architecture ref: work-design.md §3.4

use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::types::TaskBrief;

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
// §3.4 WorkPersonality
// ---------------------------------------------------------------------------

/// 定义 Agent 如何发现、选择、执行任务的行为参数。
///
/// 与 IdlePersonality 对称——前者定义「如何工作」，后者定义「如何空闲」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkPersonality {
    /// 是否启用自主认领。
    pub auto_claim: bool,

    /// 能力标签（用于匹配任务板的 skill_match）。
    pub capabilities: Vec<String>,

    /// 最大并发任务数。
    pub max_concurrent: usize,

    /// 工作冷却时间（两次巡检之间的最小间隔），单位秒。
    #[serde(with = "serde_duration_secs")]
    pub work_cooldown: Duration,

    /// 认领失败后的退避策略。
    pub claim_retry: RetryStrategy,

    /// 任务选择策略。
    pub selection: TaskSelectionStrategy,

    /// 步骤分解策略。
    pub decomposition: DecompositionStrategy,
}

impl Default for WorkPersonality {
    fn default() -> Self {
        Self {
            auto_claim: true,
            capabilities: vec!["code".into(), "refactor".into(), "fix".into(), "review".into()],
            max_concurrent: 2,
            work_cooldown: Duration::from_secs(60),
            claim_retry: RetryStrategy::default(),
            selection: TaskSelectionStrategy::default(),
            decomposition: DecompositionStrategy::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// RetryStrategy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryStrategy {
    /// 基础重试延迟，单位秒。
    #[serde(with = "serde_duration_secs")]
    pub base_delay: Duration,
    /// 退避倍数。
    pub backoff_multiplier: f64,
    /// 最大重试延迟（上限），单位秒。
    #[serde(with = "serde_duration_secs")]
    pub max_delay: Duration,
    /// 最大连续失败次数后放弃本次工作周期。
    pub max_consecutive_failures: u32,
}

impl Default for RetryStrategy {
    fn default() -> Self {
        Self {
            base_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            max_delay: Duration::from_secs(300),
            max_consecutive_failures: 5,
        }
    }
}

impl RetryStrategy {
    /// Compute the backoff delay for the given number of consecutive failures.
    #[must_use]
    pub fn backoff_delay(&self, consecutive_failures: u32) -> Duration {
        let multiplier = self.backoff_multiplier.powi(consecutive_failures as i32);
        let delay_secs = self.base_delay.as_secs_f64() * multiplier;
        let capped = delay_secs.min(self.max_delay.as_secs_f64());
        Duration::from_secs_f64(capped)
    }

    /// Whether the given number of consecutive failures has exceeded the limit.
    #[must_use]
    pub fn is_exhausted(&self, consecutive_failures: u32) -> bool {
        consecutive_failures >= self.max_consecutive_failures
    }
}

// ---------------------------------------------------------------------------
// TaskSelectionStrategy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskSelectionStrategy {
    /// 优先选择能力匹配度最高的任务。
    BestMatch,
    /// 优先选择最早创建的任务（FIFO）。
    EarliestFirst,
    /// 优先选择高优先级任务。
    HighPriorityFirst,
    /// 加权综合评分：priority * pw + match_score * mw + age * aw
    Weighted {
        priority_weight: f64,
        match_weight: f64,
        age_weight: f64,
    },
}

impl Default for TaskSelectionStrategy {
    fn default() -> Self {
        Self::Weighted {
            priority_weight: 0.4,
            match_weight: 0.4,
            age_weight: 0.2,
        }
    }
}

impl TaskSelectionStrategy {
    /// Select the best task from the available candidates.
    ///
    /// Returns `None` if the candidate list is empty.
    #[must_use]
    pub fn select(&self, candidates: &[TaskBrief], capabilities: &[String]) -> Option<TaskBrief> {
        if candidates.is_empty() {
            return None;
        }
        match self {
            Self::BestMatch => Self::select_best_match(candidates, capabilities),
            Self::EarliestFirst => Self::select_earliest(candidates),
            Self::HighPriorityFirst => Self::select_highest_priority(candidates),
            Self::Weighted {
                priority_weight,
                match_weight,
                age_weight,
            } => Self::select_weighted(
                candidates,
                capabilities,
                *priority_weight,
                *match_weight,
                *age_weight,
            ),
        }
    }

    fn select_best_match(candidates: &[TaskBrief], capabilities: &[String]) -> Option<TaskBrief> {
        candidates
            .iter()
            .max_by_key(|t| {
                t.required_capabilities
                    .iter()
                    .filter(|c| capabilities.contains(c))
                    .count()
            })
            .cloned()
    }

    fn select_earliest(candidates: &[TaskBrief]) -> Option<TaskBrief> {
        candidates.iter().min_by_key(|t| t.created_at).cloned()
    }

    fn select_highest_priority(candidates: &[TaskBrief]) -> Option<TaskBrief> {
        candidates
            .iter()
            .max_by_key(|t| priority_value(t.priority.as_deref()))
            .cloned()
    }

    fn select_weighted(
        candidates: &[TaskBrief],
        capabilities: &[String],
        priority_weight: f64,
        match_weight: f64,
        age_weight: f64,
    ) -> Option<TaskBrief> {
        let now = kernel::types::Timestamp::now();
        candidates
            .iter()
            .max_by(|a, b| {
                let score_a =
                    weighted_score(a, capabilities, priority_weight, match_weight, age_weight, now);
                let score_b =
                    weighted_score(b, capabilities, priority_weight, match_weight, age_weight, now);
                score_a
                    .partial_cmp(&score_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
    }
}

fn priority_value(priority: Option<&str>) -> u8 {
    match priority {
        Some("critical") => 5,
        Some("high") => 4,
        Some("medium") => 3,
        Some("low") => 2,
        _ => 1,
    }
}

fn match_score(task: &TaskBrief, capabilities: &[String]) -> f64 {
    if task.required_capabilities.is_empty() {
        return 0.5; // neutral match
    }
    let matched = task
        .required_capabilities
        .iter()
        .filter(|c| capabilities.contains(c))
        .count();
    matched as f64 / task.required_capabilities.len() as f64
}

fn weighted_score(
    task: &TaskBrief,
    capabilities: &[String],
    priority_weight: f64,
    match_weight: f64,
    age_weight: f64,
    now: kernel::types::Timestamp,
) -> f64 {
    let priority = f64::from(priority_value(task.priority.as_deref())) / 5.0;
    let m_score = match_score(task, capabilities);
    let age_ms = (now.as_millis() - task.created_at.as_millis()).max(0) as f64;
    // Normalize age: older tasks get higher score, cap at ~1 hour
    let age = (age_ms / 3_600_000.0).min(1.0);
    priority * priority_weight + m_score * match_weight + age * age_weight
}

// ---------------------------------------------------------------------------
// DecompositionStrategy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecompositionStrategy {
    /// 每个子步骤的最大估算耗时，单位秒。
    #[serde(with = "serde_duration_secs")]
    pub max_step_duration: Duration,
    /// 是否将 LLM 调用放在独立步骤中。
    pub isolate_llm_calls: bool,
    /// 是否将工具调用（I/O）放在独立步骤中。
    pub isolate_tool_calls: bool,
}

impl Default for DecompositionStrategy {
    fn default() -> Self {
        Self {
            max_step_duration: Duration::from_secs(120),
            isolate_llm_calls: true,
            isolate_tool_calls: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TaskId;
    use kernel::types::Timestamp;

    fn make_task(id: u8, priority: &str, capabilities: &[&str], age_ms: i64) -> TaskBrief {
        TaskBrief {
            id: TaskId(uuid::Uuid::from_u128(id as u128)),
            title: format!("task-{id}"),
            description: String::new(),
            priority: Some(priority.into()),
            required_capabilities: capabilities.iter().map(|s| s.to_string()).collect(),
            stage: "backlog".into(),
            created_at: Timestamp::from_millis(Timestamp::now().as_millis() - age_ms),
        }
    }

    #[test]
    fn retry_backoff_delay_increases() {
        let s = RetryStrategy::default();
        let d0 = s.backoff_delay(0);
        let d1 = s.backoff_delay(1);
        let d2 = s.backoff_delay(2);
        assert!(d1 > d0);
        assert!(d2 > d1);
    }

    #[test]
    fn retry_backoff_capped_at_max() {
        let s = RetryStrategy {
            base_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            max_delay: Duration::from_secs(300),
            max_consecutive_failures: 5,
        };
        let d10 = s.backoff_delay(10);
        assert_eq!(d10, Duration::from_secs(300));
    }

    #[test]
    fn retry_is_exhausted_after_limit() {
        let s = RetryStrategy::default();
        assert!(!s.is_exhausted(0));
        assert!(!s.is_exhausted(4));
        assert!(s.is_exhausted(5));
        assert!(s.is_exhausted(10));
    }

    #[test]
    fn select_empty_returns_none() {
        let strat = TaskSelectionStrategy::default();
        assert!(strat.select(&[], &["code".into()]).is_none());
    }

    #[test]
    fn select_earliest_picks_oldest() {
        let tasks = vec![
            make_task(1, "medium", &[], 5000),
            make_task(2, "low", &[], 10000),
            make_task(3, "high", &[], 1000),
        ];
        let picked = TaskSelectionStrategy::EarliestFirst
            .select(&tasks, &[])
            .expect("should pick");
        assert_eq!(
            picked.id.0.to_string(),
            "00000000-0000-0000-0000-000000000002"
        );
    }

    #[test]
    fn select_best_match_prefers_capability_overlap() {
        let tasks = vec![
            make_task(1, "medium", &["code"], 1000),
            make_task(2, "low", &["code", "review", "deploy"], 5000),
        ];
        let picked = TaskSelectionStrategy::BestMatch
            .select(&tasks, &["code".into(), "review".into()])
            .expect("should pick");
        assert_eq!(
            picked.id.0.to_string(),
            "00000000-0000-0000-0000-000000000002"
        );
    }

    #[test]
    fn select_weighted_scores_all_factors() {
        let tasks = vec![
            make_task(1, "critical", &["code"], 600_000), // old, high priority, good match
            make_task(2, "low", &[], 1000),               // new, low priority, no match
        ];
        let picked = TaskSelectionStrategy::Weighted {
            priority_weight: 0.4,
            match_weight: 0.4,
            age_weight: 0.2,
        }
        .select(&tasks, &["code".into()])
        .expect("should pick");
        assert_eq!(
            picked.id.0.to_string(),
            "00000000-0000-0000-0000-000000000001"
        );
    }
}
