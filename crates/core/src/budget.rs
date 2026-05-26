// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

/// Known context window sizes for common models (in tokens).
fn known_context_windows() -> Vec<(&'static str, usize)> {
    vec![
        ("gpt-4o", 128_000),
        ("gpt-4o-mini", 128_000),
        ("gpt-4-turbo", 128_000),
        ("gpt-4", 8_192),
        ("gpt-4-32k", 32_768),
        ("gpt-3.5-turbo", 16_385),
        ("gpt-3.5-turbo-16k", 16_385),
        ("claude-opus-4-5", 200_000),
        ("claude-opus-4-7", 200_000),
        ("claude-sonnet-4-6", 200_000),
        ("claude-haiku-4-5", 200_000),
        ("deepseek-chat", 128_000),
        ("deepseek-v4", 1_048_576),
        ("deepseek-v4-pro", 1_048_576),
        ("deepseek-r1", 128_000),
        ("gemini-pro", 32_768),
        ("gemini-ultra", 32_768),
        ("llama-3-70b", 8_192),
        ("llama-3-8b", 8_192),
        ("mistral-large", 32_768),
        ("mistral-medium", 32_768),
        ("qwen-max", 32_768),
    ]
}

/// Resolve context window by exact or prefix match.
fn default_context_window_for_model(model: &str) -> usize {
    for (key, size) in known_context_windows() {
        if model == key || model.starts_with(key) || model.contains(key) {
            return size;
        }
    }
    if model.starts_with("gpt-") {
        return 8_192;
    }
    if model.starts_with("claude-") {
        return 100_000;
    }
    if model.starts_with("deepseek-") {
        return 64_000;
    }
    32_768
}

/// Pluggable token budget policy for ReAct sessions.
///
/// Determines the session-level token budget, model context window,
/// and output token limit. The default implementation mirrors the
/// original hardcoded behavior with known model mappings.
pub trait TokenBudgetPolicy: Send + Sync {
    /// Maximum tokens allowed across the entire session.
    fn session_token_limit(&self) -> u64;

    /// Look up the context window size for a given model name.
    fn context_window(&self, model: &str) -> usize;

    /// Maximum output tokens per LLM call.
    /// `agent_value` is the agent config's max_output_tokens, if set.
    fn max_output_tokens(&self, model: &str, agent_value: Option<usize>) -> usize;
}

/// Default policy matching the original hardcoded behavior.
pub struct DefaultTokenBudgetPolicy {
    session_token_limit: u64,
}

impl DefaultTokenBudgetPolicy {
    pub fn new() -> Self {
        Self {
            session_token_limit: 100_000,
        }
    }

    pub fn with_session_limit(limit: u64) -> Self {
        Self {
            session_token_limit: limit,
        }
    }
}

impl Default for DefaultTokenBudgetPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenBudgetPolicy for DefaultTokenBudgetPolicy {
    fn session_token_limit(&self) -> u64 {
        self.session_token_limit
    }

    fn context_window(&self, model: &str) -> usize {
        default_context_window_for_model(model)
    }

    fn max_output_tokens(&self, _model: &str, agent_value: Option<usize>) -> usize {
        agent_value.unwrap_or(0)
    }
}
