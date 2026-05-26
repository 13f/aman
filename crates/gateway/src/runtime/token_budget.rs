#![allow(dead_code)]

// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use std::collections::HashMap;

/// Known context window sizes for common models (in tokens).
fn known_context_windows() -> HashMap<&'static str, usize> {
    let mut m = HashMap::new();
    m.insert("gpt-4o", 128_000);
    m.insert("gpt-4o-mini", 128_000);
    m.insert("gpt-4-turbo", 128_000);
    m.insert("gpt-4", 8_192);
    m.insert("gpt-4-32k", 32_768);
    m.insert("gpt-3.5-turbo", 16_385);
    m.insert("gpt-3.5-turbo-16k", 16_385);
    m.insert("claude-opus-4-5", 200_000);
    m.insert("claude-opus-4-7", 200_000);
    m.insert("claude-sonnet-4-6", 200_000);
    m.insert("claude-haiku-4-5", 200_000);
    m.insert("deepseek-chat", 128_000);
    m.insert("deepseek-v4", 1_048_576);
    m.insert("deepseek-v4-pro", 1_048_576);
    m.insert("deepseek-r1", 128_000);
    m.insert("gemini-pro", 32_768);
    m.insert("gemini-ultra", 32_768);
    m.insert("llama-3-70b", 8_192);
    m.insert("llama-3-8b", 8_192);
    m.insert("mistral-large", 32_768);
    m.insert("mistral-medium", 32_768);
    m.insert("qwen-max", 32_768);
    // Default for unknown models
    m
}

/// Resolve the context window size for a given model name.
pub fn context_window_for_model(model: &str) -> usize {
    let known = known_context_windows();
    // Try exact match first, then prefix match
    if let Some(&size) = known.get(model) {
        return size;
    }
    for (key, &size) in &known {
        if model.starts_with(key) || model.contains(key) {
            return size;
        }
    }
    // Some common prefixes
    if model.starts_with("gpt-") {
        return 8_192;
    }
    if model.starts_with("claude-") {
        return 100_000;
    }
    if model.starts_with("deepseek-") {
        return 64_000;
    }
    // Conservative default
    32_768
}

/// Token budget tracker with model-aware context window management.
///
/// Tracks per-component token usage (system, tool schemas, history, outputs)
/// and determines when history needs to be compressed. Uses a threshold-based
/// trigger (e.g. 80% of context window) with anti-thrashing to prevent
/// repeated ineffective compressions.
#[derive(Clone)]
#[allow(dead_code)]
pub struct TokenBudget {
    /// Model name used for context window lookup.
    pub model: String,
    /// Maximum context window size in tokens.
    pub context_window: usize,
    /// Tokens reserved for model output per turn.
    pub max_output_tokens: usize,
    /// Maximum tokens available for the prompt (context_window - max_output_tokens).
    pub max_prompt_tokens: usize,

    /// Current token count of conversation history.
    pub current_history_tokens: usize,
    /// Current token count of tool schemas in the prompt.
    pub current_tool_schema_tokens: usize,
    /// Current token count of system prompt (SOUL).
    pub current_system_tokens: usize,

    // ── Threshold & anti-thrashing ──
    /// Compression threshold (0.0–1.0). Triggers when prompt tokens reach
    /// this fraction of the context window. Default: 0.80.
    pub compression_threshold: f64,
    /// Number of consecutive ineffective compressions (below min_savings_pct).
    pub ineffective_compression_count: u8,
    /// When true, compression is paused due to anti-thrashing.
    pub compression_paused: bool,
    /// Tokens saved by the most recent compression run.
    pub last_compression_savings: usize,
    /// Total prompt tokens recorded before the current compression started.
    pub pre_compression_total: usize,
}

impl TokenBudget {
    /// Create a new TokenBudget for the given model.
    /// Uses a default max_output_tokens of 0 (must be set from config).
    pub fn new(model: impl Into<String>) -> Self {
        let model = model.into();
        let context_window = context_window_for_model(&model);
        let max_output_tokens = 0;
        let max_prompt_tokens = context_window.saturating_sub(max_output_tokens);
        Self {
            model,
            context_window,
            max_output_tokens,
            max_prompt_tokens,
            current_history_tokens: 0,
            current_tool_schema_tokens: 0,
            current_system_tokens: 0,
            compression_threshold: 0.80,
            ineffective_compression_count: 0,
            compression_paused: false,
            last_compression_savings: 0,
            pre_compression_total: 0,
        }
    }

    /// Create with explicit context window (bypasses model lookup).
    pub fn with_window(model: impl Into<String>, context_window: usize, max_output_tokens: usize) -> Self {
        let model = model.into();
        let max_prompt_tokens = context_window.saturating_sub(max_output_tokens);
        Self {
            model,
            context_window,
            max_output_tokens,
            max_prompt_tokens,
            current_history_tokens: 0,
            current_tool_schema_tokens: 0,
            current_system_tokens: 0,
            compression_threshold: 0.80,
            ineffective_compression_count: 0,
            compression_paused: false,
            last_compression_savings: 0,
            pre_compression_total: 0,
        }
    }

    /// Estimate the number of tokens in a text string.
    /// Uses a conservative chars-to-tokens ratio to avoid underestimation
    /// (code, JSON, and CJK text have higher token density than English prose).
    pub fn estimate_tokens(text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        (text.len() / 3).max(1)
    }

    /// Current total prompt usage (system + tool schemas + history).
    pub fn total_prompt_tokens(&self) -> usize {
        self.current_system_tokens
            + self.current_tool_schema_tokens
            + self.current_history_tokens
    }

    /// Token usage as a percentage of the context window (0–100).
    pub fn usage_percent(&self) -> f64 {
        if self.context_window == 0 {
            return 0.0;
        }
        (self.total_prompt_tokens() as f64 / self.context_window as f64) * 100.0
    }

    /// Check whether history needs compression.
    ///
    /// Returns true when total prompt tokens reach the compression threshold
    /// (default 80% of context window). When anti-thrashing has paused
    /// compression, only the hard limit (>95%) will trigger.
    pub fn needs_trim(&self) -> bool {
        if self.compression_paused {
            // Safety valve: force compress if above 95% regardless of pause
            let hard_limit = (self.context_window as f64 * 0.95) as usize;
            return self.total_prompt_tokens() > hard_limit;
        }
        let threshold_tokens = (self.context_window as f64 * self.compression_threshold) as usize;
        self.total_prompt_tokens() >= threshold_tokens
    }

    /// Number of tokens to remove to get below the trigger threshold.
    pub fn trim_amount(&self) -> usize {
        let target = (self.context_window as f64 * self.compression_threshold) as usize;
        self.total_prompt_tokens().saturating_sub(target)
    }

    /// Quick estimate whether the given messages would exceed the threshold.
    /// Call before sending to the LLM API to avoid 400 errors.
    pub fn preflight_check(&self, messages: &[kernel::react::ChatMessage]) -> bool {
        let estimated_tokens: usize = messages
            .iter()
            .map(|m| Self::estimate_tokens(&m.content))
            .sum();
        let extra = self.current_system_tokens + self.current_tool_schema_tokens;
        let threshold = (self.context_window as f64 * self.compression_threshold) as usize;
        estimated_tokens.saturating_add(extra) >= threshold
    }

    /// Record token usage from an LLM call (for tracking, not trim logic).
    pub fn record_usage(&mut self, prompt_tokens: usize, completion_tokens: usize) {
        self.current_history_tokens = self
            .current_history_tokens
            .saturating_add(prompt_tokens)
            .saturating_add(completion_tokens);
    }

    /// Snapshot current total before a compression run (for savings calc).
    pub fn start_compression(&mut self) {
        self.pre_compression_total = self.total_prompt_tokens();
    }

    /// Record the result of a compression run. Updates anti-thrashing state:
    /// pauses compression after 2 consecutive ineffective runs.
    pub fn record_compression(&mut self, tokens_saved: usize) {
        if self.pre_compression_total > 0 {
            let savings_pct = (tokens_saved as f64 / self.pre_compression_total as f64) * 100.0;
            if savings_pct < 10.0 {
                self.ineffective_compression_count += 1;
            } else {
                self.ineffective_compression_count = 0;
            }
            if self.ineffective_compression_count >= 2 {
                self.compression_paused = true;
            }
        }
        self.last_compression_savings = tokens_saved;
    }

    /// Reset all anti-thrashing state (e.g. on user-initiated /compress).
    pub fn reset_compression_state(&mut self) {
        self.ineffective_compression_count = 0;
        self.compression_paused = false;
        self.last_compression_savings = 0;
        self.pre_compression_total = 0;
    }

    /// Set the system prompt token count.
    pub fn set_system_tokens(&mut self, tokens: usize) {
        self.current_system_tokens = tokens;
    }

    /// Set the tool schema token count.
    pub fn set_tool_schema_tokens(&mut self, tokens: usize) {
        self.current_tool_schema_tokens = tokens;
    }

    /// Set history tokens directly (e.g., after trimming).
    pub fn set_history_tokens(&mut self, tokens: usize) {
        self.current_history_tokens = tokens;
    }

    /// Fraction of the prompt budget used (0.0 – 1.0).
    pub fn prompt_fraction(&self) -> f64 {
        if self.max_prompt_tokens == 0 {
            return 0.0;
        }
        (self.total_prompt_tokens() as f64) / (self.max_prompt_tokens as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_model_context_window() {
        assert_eq!(context_window_for_model("gpt-4o"), 128_000);
        assert_eq!(context_window_for_model("claude-opus-4-7"), 200_000);
        assert_eq!(context_window_for_model("deepseek-v4"), 1_048_576);
        assert_eq!(context_window_for_model("deepseek-v4-pro"), 1_048_576);
    }

    #[test]
    fn test_fallback_context_window() {
        let w = context_window_for_model("unknown-model-x7");
        assert_eq!(w, 32_768);
    }

    #[test]
    fn test_prefix_matching() {
        assert_eq!(context_window_for_model("gpt-5"), 8_192);
        assert_eq!(context_window_for_model("claude-4-sonnet"), 100_000);
    }

    #[test]
    fn test_token_budget_new() {
        let budget = TokenBudget::new("gpt-4o");
        assert_eq!(budget.model, "gpt-4o");
        assert_eq!(budget.context_window, 128_000);
        assert_eq!(budget.max_output_tokens, 0);
        assert_eq!(budget.max_prompt_tokens, 128_000);
        assert_eq!(budget.compression_threshold, 0.80);
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(TokenBudget::estimate_tokens("hello"), 1);
        assert_eq!(TokenBudget::estimate_tokens(""), 0);
        let long = "a".repeat(100);
        assert_eq!(TokenBudget::estimate_tokens(&long), 33);
    }

    #[test]
    fn test_needs_trim_threshold() {
        let mut budget = TokenBudget::with_window("test", 1000, 0);
        // Threshold = 1000 * 0.80 = 800. At 799, no trim.
        budget.current_history_tokens = 799;
        assert!(!budget.needs_trim());
        // At 800, trim triggers.
        budget.current_history_tokens = 800;
        assert!(budget.needs_trim());
    }

    #[test]
    fn test_needs_trim_below_threshold() {
        let budget = TokenBudget::with_window("test", 1000, 0);
        // No history → far below 80%
        assert!(!budget.needs_trim());
    }

    #[test]
    fn test_hard_limit_bypasses_pause() {
        let mut budget = TokenBudget::with_window("test", 1000, 0);
        budget.compression_paused = true;
        // At 95% + 1 (= 951), hard limit triggers even when paused
        budget.current_history_tokens = 951;
        assert!(budget.needs_trim());
        // Below hard limit, paused stays false
        budget.current_history_tokens = 940;
        assert!(!budget.needs_trim());
    }

    #[test]
    fn test_preflight_check() {
        let mut budget = TokenBudget::with_window("test", 1000, 0);
        budget.set_system_tokens(100);
        // 100 (system) + estimated tokens from messages
        let messages = vec![
            kernel::react::ChatMessage::user("a".repeat(3000)), // ~1000 est tokens
        ];
        // 100 system + 1000 history = 1100 >= 800 → true
        assert!(budget.preflight_check(&messages));
        let empty: Vec<kernel::react::ChatMessage> = vec![];
        assert!(!budget.preflight_check(&empty));
    }

    #[test]
    fn test_anti_thrashing() {
        let mut budget = TokenBudget::with_window("test", 1000, 0);
        budget.current_history_tokens = 1000;
        budget.start_compression();
        // Save only 5% → ineffective
        budget.record_compression(50);
        assert_eq!(budget.ineffective_compression_count, 1);
        assert!(!budget.compression_paused);

        budget.start_compression();
        // Save only 3% again → second ineffective, pause triggers
        budget.record_compression(30);
        assert_eq!(budget.ineffective_compression_count, 2);
        assert!(budget.compression_paused);
    }

    #[test]
    fn test_anti_thrashing_reset() {
        let mut budget = TokenBudget::with_window("test", 1000, 0);
        budget.current_history_tokens = 1000;
        budget.start_compression();
        budget.record_compression(50); // ineffective #1
        assert_eq!(budget.ineffective_compression_count, 1);

        budget.start_compression();
        budget.record_compression(200); // good save → resets counter
        assert_eq!(budget.ineffective_compression_count, 0);
    }

    #[test]
    fn test_record_usage() {
        let mut budget = TokenBudget::with_window("test", 1000, 200);
        assert_eq!(budget.current_history_tokens, 0);
        budget.record_usage(100, 50);
        assert_eq!(budget.current_history_tokens, 150);
        budget.record_usage(200, 30);
        assert_eq!(budget.current_history_tokens, 380);
    }
}
