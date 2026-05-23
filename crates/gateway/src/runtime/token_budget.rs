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
    m.insert("deepseek-v4", 128_000);
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
/// and determines when history needs to be compressed.
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
        }
    }

    /// Estimate the number of tokens in a text string.
    /// Uses the approximation: text.len() / 4 + 1 (chars -> tokens ratio for English).
    pub fn estimate_tokens(text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }
        (text.len() / 4).max(1)
    }

    /// Current total prompt usage (system + tool schemas + history).
    pub fn total_prompt_tokens(&self) -> usize {
        self.current_system_tokens
            + self.current_tool_schema_tokens
            + self.current_history_tokens
    }

    /// Check whether history needs trimming.
    pub fn needs_trim(&self) -> bool {
        self.total_prompt_tokens() > self.max_prompt_tokens
    }

    /// Number of tokens that need to be trimmed to get below the threshold.
    /// Uses an 80% target threshold for a safety margin.
    pub fn trim_amount(&self) -> usize {
        let target = (self.max_prompt_tokens as f64 * 0.8) as usize;
        self.total_prompt_tokens().saturating_sub(target)
    }

    /// Record token usage from an LLM call.
    pub fn record_usage(&mut self, prompt_tokens: usize, completion_tokens: usize) {
        self.current_history_tokens = self
            .current_history_tokens
            .saturating_add(prompt_tokens)
            .saturating_add(completion_tokens);
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
        assert_eq!(context_window_for_model("deepseek-v4"), 128_000);
    }

    #[test]
    fn test_fallback_context_window() {
        // Unknown model gets conservative default
        let w = context_window_for_model("unknown-model-x7");
        assert_eq!(w, 32_768);
    }

    #[test]
    fn test_prefix_matching() {
        // gpt- prefix fallback
        assert_eq!(context_window_for_model("gpt-5"), 8_192);
        // claude- prefix fallback
        assert_eq!(context_window_for_model("claude-4-sonnet"), 100_000);
    }

    #[test]
    fn test_token_budget_new() {
        let budget = TokenBudget::new("gpt-4o");
        assert_eq!(budget.model, "gpt-4o");
        assert_eq!(budget.context_window, 128_000);
        assert_eq!(budget.max_output_tokens, 0);
        assert_eq!(budget.max_prompt_tokens, 128_000);
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(TokenBudget::estimate_tokens("hello"), 1);
        assert_eq!(TokenBudget::estimate_tokens(""), 0);
        let long = "a".repeat(100);
        assert_eq!(TokenBudget::estimate_tokens(&long), 25);
    }

    #[test]
    fn test_needs_trim() {
        let mut budget = TokenBudget::with_window("test", 1000, 200);
        budget.current_history_tokens = 700;
        // 700 < 800 (1000-200) → no trim
        assert!(!budget.needs_trim());

        budget.current_history_tokens = 900;
        // 900 > 800 → needs trim
        assert!(budget.needs_trim());
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
