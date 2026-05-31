#![forbid(unsafe_code)]
#![doc = "Context Manager — fights context rot via token budgeting, structured sections, priority eviction, and rot detection."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

mod compressor;
mod priority;
mod rot;
mod section;
mod token_budget;

pub use compressor::{CompressResult, CompressionStrategy, CompressorConfig, HistoryCompressor};
pub use priority::{ContextPriority, PriorityScorer};
pub use rot::{RotDetector, RotSignal};
pub use section::{ContextSection, ContextWindow, SectionBudget};
pub use token_budget::{
    context_window_for_model, DefaultTokenBudgetPolicy, TokenBudget, TokenBudgetPolicy,
};

use kernel::memory::MemoryProvider;
use kernel::react::ChatMessage;
use kernel::session_history::SessionHistoryStore;
use std::sync::Arc;

/// Result of a pre-turn context preparation pass.
#[derive(Debug, Clone)]
pub struct PreTurnResult {
    /// Whether compression was applied this turn.
    pub compressed: bool,
    /// Messages removed during compression (if any).
    pub messages_removed: usize,
    /// Tokens saved during compression (if any).
    pub tokens_saved: usize,
    /// Current token usage as a percentage of the context window.
    pub usage_percent: f64,
    /// Whether anti-thrashing has paused further compression.
    pub compression_paused: bool,
    /// Detected context rot signals (if any).
    pub rot_signals: Vec<RotSignal>,
}

/// The central context management orchestrator.
///
/// Composes token budgeting, history compression, structured context sections,
/// priority-based eviction, and rot detection into a single coherent component —
/// the **C** in the Harness model H = (E, T, C, S, L, V).
pub struct ContextManager {
    /// Model-aware token budget tracker.
    pub budget: TokenBudget,
    /// History compression engine.
    compressor: HistoryCompressor,
    /// Compression configuration.
    compressor_config: CompressorConfig,
    /// Per-session conversation history store.
    session_history: Box<dyn SessionHistoryStore>,
    /// Optional memory provider for dynamic retrieval.
    memory_provider: Option<Arc<dyn MemoryProvider>>,
    /// Context rot detector.
    rot_detector: RotDetector,
    /// Priority scorer for eviction decisions.
    priority_scorer: PriorityScorer,
}

impl ContextManager {
    /// Create a new ContextManager.
    pub fn new(
        session_history: Box<dyn SessionHistoryStore>,
        compressor_config: CompressorConfig,
        memory_provider: Option<Arc<dyn MemoryProvider>>,
    ) -> Self {
        Self {
            budget: TokenBudget::new("default"),
            compressor: HistoryCompressor::new(CompressionStrategy::Truncate),
            compressor_config,
            session_history,
            memory_provider,
            rot_detector: RotDetector::new(),
            priority_scorer: PriorityScorer::new(),
        }
    }

    /// Initialise the token budget for a session.
    ///
    /// Call once at the start of `process_message`, after the agent model
    /// and configuration are known.
    pub fn init_budget(
        &mut self,
        model: &str,
        context_window: usize,
        max_output_tokens: usize,
        system_prompt: &str,
        tool_schemas_text: &str,
        history: &[ChatMessage],
    ) {
        self.budget = TokenBudget::with_window(model, context_window, max_output_tokens);
        self.budget
            .set_system_tokens(TokenBudget::estimate_tokens(system_prompt));
        self.budget
            .set_tool_schema_tokens(TokenBudget::estimate_tokens(tool_schemas_text));
        let history_tokens: usize = history
            .iter()
            .map(|m| TokenBudget::estimate_tokens(&m.content))
            .sum();
        self.budget.set_history_tokens(history_tokens);
    }

    /// Called before each ReAct turn.
    ///
    /// Re-estimates history tokens, applies compression if needed,
    /// and runs rot detection. Returns a summary of actions taken.
    pub fn pre_turn(&mut self, history: &mut Vec<ChatMessage>, turn: u32) -> PreTurnResult {
        let mut result = PreTurnResult {
            compressed: false,
            messages_removed: 0,
            tokens_saved: 0,
            usage_percent: self.budget.usage_percent(),
            compression_paused: self.budget.compression_paused,
            rot_signals: Vec::new(),
        };

        // Re-estimate history tokens
        let history_tokens: usize = history
            .iter()
            .map(|m| TokenBudget::estimate_tokens(&m.content))
            .sum();
        self.budget.set_history_tokens(history_tokens);

        // Check if compression is needed
        if self.budget.needs_trim() {
            let config = self.compressor_config.clone();
            let compress_result = self
                .compressor
                .compress_with_boundaries(history, &mut self.budget, &config);
            result.compressed = true;
            result.messages_removed = compress_result.messages_removed;
            result.tokens_saved = compress_result.tokens_saved;
            result.usage_percent = self.budget.usage_percent();
            result.compression_paused = self.budget.compression_paused;
        }

        // Preflight: catch oversized requests that slipped past the threshold check
        if self.budget.preflight_check(history) {
            let config = self.compressor_config.clone();
            let compress_result = self
                .compressor
                .compress_with_boundaries(history, &mut self.budget, &config);
            if compress_result.messages_removed > 0 || compress_result.tokens_saved > 0 {
                result.compressed = true;
                result.messages_removed += compress_result.messages_removed;
                result.tokens_saved += compress_result.tokens_saved;
                result.usage_percent = self.budget.usage_percent();
                result.compression_paused = self.budget.compression_paused;
            }
        }

        // Run rot detection on the current history
        let signals = self.rot_detector.detect(history, turn);
        result.rot_signals = signals;

        result
    }

    /// Called after each ReAct turn completes.
    ///
    /// Feeds the turn outcome into the rot detector and updates priority
    /// scores based on observed utility.
    pub fn post_turn(&mut self, content: &str, tool_calls: &[kernel::react::ParsedToolCall]) {
        self.rot_detector
            .feed_turn(content, tool_calls);
        // Update priority scorer with turn outcome
        self.priority_scorer.record_turn(content, tool_calls);
    }

    /// Record token usage from a completed LLM call.
    pub fn record_usage(&mut self, completion_tokens: usize) {
        self.budget.record_usage(0, completion_tokens);
    }

    /// Retrieve memories relevant to the current context.
    ///
    /// Unlike the one-shot retrieval at session start, this can be called
    /// mid-session to pull fresh memories as the conversation evolves.
    pub async fn refresh_memories(
        &self,
        agent_id: &str,
        query: &str,
    ) -> Option<String> {
        let provider = self.memory_provider.as_ref()?;
        let results = provider.recall(agent_id, query, 5).await;
        if results.is_empty() {
            return None;
        }
        let mem_text: Vec<String> = results
            .iter()
            .map(|m| format!("- {} (tags: {})", m.content, m.tags.join(", ")))
            .collect();
        Some(mem_text.join("\n"))
    }

    /// Access the session history store.
    pub fn session_history(&self) -> &dyn SessionHistoryStore {
        self.session_history.as_ref()
    }

    /// Estimate tokens in a text string (convenience pass-through).
    pub fn estimate_tokens(text: &str) -> usize {
        TokenBudget::estimate_tokens(text)
    }
}
