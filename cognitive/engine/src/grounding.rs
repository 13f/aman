// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Grounding translator — evaluates the agent's "information readiness".
//!
//! Two independent dimensions:
//! - **Knowledge**: Does the agent have relevant domain knowledge?
//!   (computed from memory retrieval results)
//! - **Situation**: Is the user's request clear and well-formed?
//!   (computed from the user's message text)
//!
//! The translator is **pure logic** — no LLM, no I/O. The gateway computes
//! raw signals and passes them in.

use crate::context::{KnowledgeSignal, SituationSignal};

/// Raw signals for knowledge evaluation.
#[derive(Debug, Clone, Copy, Default)]
pub struct KnowledgeInput {
    /// Number of memory records retrieved.
    pub memory_count: usize,
    /// Average importance of retrieved memories (0.0–1.0).
    pub avg_importance: f64,
    /// Average age of retrieved memories in days (None if no timestamp).
    pub avg_age_days: Option<f64>,
    /// Number of distinct domains covered by retrieved memories.
    pub domain_count: usize,
}

/// Raw signals for situation evaluation.
#[derive(Debug, Clone, Default)]
pub struct SituationInput {
    /// The user's message text.
    pub user_text: String,
    /// Current context token count (conversation + memories).
    pub context_tokens: usize,
    /// Token budget for this session.
    pub token_budget: usize,
}

/// Thresholds for knowledge classification.
#[derive(Debug, Clone, Copy)]
pub struct KnowledgeThresholds {
    /// Minimum memory count to be considered "informed".
    pub min_count: usize,
    /// Minimum average importance to be considered "informed".
    pub min_importance: f64,
    /// Age in days above which knowledge is considered "outdated".
    pub max_age_days: f64,
}

impl Default for KnowledgeThresholds {
    fn default() -> Self {
        Self {
            min_count: 3,
            min_importance: 0.3,
            max_age_days: 30.0,
        }
    }
}

/// Thresholds for situation classification.
#[derive(Debug, Clone, Copy)]
pub struct SituationThresholds {
    /// Minimum tokens for a "clear" request (below this → Vague).
    pub min_text_tokens: usize,
    /// Context token ratio above which situation is "Overloaded".
    pub overload_ratio: f64,
}

impl Default for SituationThresholds {
    fn default() -> Self {
        Self {
            min_text_tokens: 20,
            overload_ratio: 0.7,
        }
    }
}

/// Evaluate knowledge dimension from raw signals.
pub fn evaluate_knowledge(
    input: &KnowledgeInput,
    thresholds: KnowledgeThresholds,
) -> KnowledgeSignal {
    // Not enough memories → uninformed
    if input.memory_count < thresholds.min_count {
        return KnowledgeSignal::Uninformed;
    }

    // Low importance → uninformed
    if input.avg_importance < thresholds.min_importance {
        return KnowledgeSignal::Uninformed;
    }

    // Check staleness
    if let Some(age) = input.avg_age_days
        && age > thresholds.max_age_days
    {
        return KnowledgeSignal::Outdated;
    }

    KnowledgeSignal::Informed
}

/// Evaluate situation dimension from raw signals.
pub fn evaluate_situation(
    input: &SituationInput,
    thresholds: SituationThresholds,
) -> SituationSignal {
    // Check overload first — too much context
    if input.token_budget > 0 {
        let ratio = input.context_tokens as f64 / input.token_budget as f64;
        if ratio > thresholds.overload_ratio {
            return SituationSignal::Overloaded;
        }
    }

    // Check vagueness — message too short
    let token_estimate = estimate_tokens(&input.user_text);
    let char_count = input.user_text.chars().count();
    if token_estimate < thresholds.min_text_tokens {
        // Very short messages (< 8 chars) are always vague — too little info
        if char_count < 8 {
            return SituationSignal::Vague;
        }
        // Short messages need an action verb to be considered clear
        if !has_action_verb(&input.user_text) {
            return SituationSignal::Vague;
        }
    }

    SituationSignal::Clear
}

/// Rough token estimation (1 token ≈ 4 chars for English, 1-2 chars for CJK).
fn estimate_tokens(text: &str) -> usize {
    let char_count = text.chars().count();
    let cjk_count = text.chars().filter(|c| is_cjk(*c)).count();
    let non_cjk = char_count - cjk_count;
    // CJK: ~1.5 chars/token, non-CJK: ~4 chars/token
    let tokens = (cjk_count as f64 / 1.5 + non_cjk as f64 / 4.0) as usize;
    tokens.max(1)
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' |  // CJK Unified Ideographs
        '\u{3400}'..='\u{4DBF}' |  // CJK Extension A
        '\u{F900}'..='\u{FAFF}' |  // CJK Compatibility Ideographs
        '\u{3040}'..='\u{309F}' |  // Hiragana
        '\u{30A0}'..='\u{30FF}'     // Katakana
    )
}

/// Check if the text contains an action verb (simple heuristic).
fn has_action_verb(text: &str) -> bool {
    let lower = text.to_lowercase();
    const VERBS: &[&str] = &[
        "analyze", "create", "delete", "update", "fix", "add", "remove",
        "search", "find", "get", "list", "show", "tell", "explain",
        "compare", "deploy", "build", "test", "write", "read", "check",
        "help", "make", "send", "open", "close", "start", "stop",
        "分析", "创建", "删除", "更新", "修复", "添加", "搜索",
        "查找", "显示", "解释", "比较", "部署", "构建", "测试",
        "写", "读", "检查", "帮助", "发送", "打开", "关闭",
    ];
    VERBS.iter().any(|v| lower.contains(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_knowledge_informed() {
        let input = KnowledgeInput {
            memory_count: 5,
            avg_importance: 0.6,
            avg_age_days: Some(10.0),
            domain_count: 2,
        };
        assert_eq!(evaluate_knowledge(&input, KnowledgeThresholds::default()), KnowledgeSignal::Informed);
    }

    #[test]
    fn test_knowledge_uninformed_low_count() {
        let input = KnowledgeInput {
            memory_count: 1,
            avg_importance: 0.8,
            avg_age_days: Some(5.0),
            domain_count: 1,
        };
        assert_eq!(evaluate_knowledge(&input, KnowledgeThresholds::default()), KnowledgeSignal::Uninformed);
    }

    #[test]
    fn test_knowledge_outdated() {
        let input = KnowledgeInput {
            memory_count: 10,
            avg_importance: 0.7,
            avg_age_days: Some(60.0),
            domain_count: 3,
        };
        assert_eq!(evaluate_knowledge(&input, KnowledgeThresholds::default()), KnowledgeSignal::Outdated);
    }

    #[test]
    fn test_situation_clear() {
        let input = SituationInput {
            user_text: "Please analyze the performance metrics for the last quarter".into(),
            context_tokens: 500,
            token_budget: 4096,
        };
        assert_eq!(evaluate_situation(&input, SituationThresholds::default()), SituationSignal::Clear);
    }

    #[test]
    fn test_situation_vague() {
        let input = SituationInput {
            user_text: "help".into(),
            context_tokens: 100,
            token_budget: 4096,
        };
        assert_eq!(evaluate_situation(&input, SituationThresholds::default()), SituationSignal::Vague);
    }

    #[test]
    fn test_situation_overloaded() {
        let input = SituationInput {
            user_text: "Please analyze the performance metrics for the last quarter".into(),
            context_tokens: 3500,
            token_budget: 4096,
        };
        assert_eq!(evaluate_situation(&input, SituationThresholds::default()), SituationSignal::Overloaded);
    }

    #[test]
    fn test_cjk_token_estimation() {
        let tokens = estimate_tokens("请帮我分析这个文件");
        assert!((3..=8).contains(&tokens));
    }
}
