// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::react::ChatMessage;

use crate::runtime::token_budget::TokenBudget;

/// Compression strategy for reducing conversation history when token budget is exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum CompressionStrategy {
    /// Drop the oldest messages until under threshold.
    Truncate,
    /// Drop the oldest messages until under threshold.
    /// (Summarize is reserved for future LLM-based compression.)
    Summarize,
}

impl CompressionStrategy {
    pub fn is_truncate(&self) -> bool {
        matches!(self, Self::Truncate)
    }
}

/// Handles compression of conversation history when token budget is exceeded.
pub struct HistoryCompressor {
    strategy: CompressionStrategy,
}

impl HistoryCompressor {
    pub fn new(strategy: CompressionStrategy) -> Self {
        Self { strategy }
    }

    /// Try to compress history by removing the oldest user+assistant message pairs.
    ///
    /// Returns the number of messages removed and the estimated tokens saved.
    /// Keeps at least `min_messages` messages to maintain context.
    pub fn compress(
        &self,
        history: &mut Vec<ChatMessage>,
        budget: &mut TokenBudget,
        min_messages: usize,
    ) -> CompressResult {
        if !budget.needs_trim() || history.len() <= min_messages {
            return CompressResult {
                messages_removed: 0,
                tokens_saved: 0,
                strategy: self.strategy,
            };
        }

        let amount_to_save = budget.trim_amount();

        match self.strategy {
            CompressionStrategy::Truncate => {
                self.truncate(history, budget, amount_to_save, min_messages)
            }
            CompressionStrategy::Summarize => {
                // For now, summarize uses the same truncate logic.
                // Future: call LLM to summarize oldest messages.
                self.truncate(history, budget, amount_to_save, min_messages)
            }
        }
    }

    /// Remove oldest messages until the target token savings are achieved
    /// and at least `min_messages` remain.
    ///
    /// Messages that start with `[ACTIVATED SKILL:` or `[The skill` are
    /// protected (skill activation messages) and will be skipped rather than
    /// removed — this prevents the LLM from losing the skill's output template
    /// and instructions during long ReAct runs.
    fn truncate(
        &self,
        history: &mut Vec<ChatMessage>,
        budget: &mut TokenBudget,
        target_save: usize,
        min_messages: usize,
    ) -> CompressResult {
        let mut removed = 0usize;
        let mut tokens_saved = 0usize;

        // Remove from the front (oldest first), protecting skill activation
        // messages and system messages from truncation.
        while history.len() > min_messages && tokens_saved < target_save {
            let is_protected = is_skill_activation(&history[0].content);
            if is_protected {
                // Move protected messages to the end so they survive truncation.
                // This keeps the skill template accessible for the final output.
                let msg = history.remove(0);
                history.push(msg);
                if history.len() <= min_messages {
                    break;
                }
                continue;
            }
            let oldest = history.remove(0);
            let tokens = TokenBudget::estimate_tokens(&oldest.content);
            tokens_saved += tokens;
            removed += 1;
        }

        // Update the budget's history token count (estimate what remains)
        let remaining_tokens: usize = history
            .iter()
            .map(|m| TokenBudget::estimate_tokens(&m.content))
            .sum();
        budget.set_history_tokens(remaining_tokens);

        CompressResult {
            messages_removed: removed,
            tokens_saved,
            strategy: self.strategy,
        }
    }
}

/// Result of a compression operation.
#[derive(Debug, Clone)]
pub struct CompressResult {
    pub messages_removed: usize,
    pub tokens_saved: usize,
    pub strategy: CompressionStrategy,
}

/// Check if a message contains skill activation content that should survive truncation.
///
/// Skill activation messages carry the full SKILL.md template and instructions
/// that the LLM needs to produce correctly formatted output. Truncating them
/// causes the final report to lose structure.
fn is_skill_activation(content: &str) -> bool {
    content.starts_with("[ACTIVATED SKILL:")
        || content.starts_with("[The skill \"")
        || content.starts_with("[FORMAT INSTRUCTION]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_history(count: usize) -> Vec<ChatMessage> {
        (0..count)
            .map(|i| {
                if i % 2 == 0 {
                    ChatMessage::user(format!("user message {i}"))
                } else {
                    ChatMessage::assistant(format!("assistant reply {i}"))
                }
            })
            .collect()
    }

    #[test]
    fn test_no_compression_when_under_budget() {
        let mut history = make_history(4);
        let mut budget = TokenBudget::with_window("test", 1000, 200);
        budget.set_history_tokens(100); // well under 800 max_prompt

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let result = compressor.compress(&mut history, &mut budget, 2);

        assert_eq!(result.messages_removed, 0);
        assert_eq!(history.len(), 4);
    }

    #[test]
    fn test_truncate_removes_oldest() {
        let mut history = make_history(10);
        let mut budget = TokenBudget::with_window("test", 1000, 200);
        // Set history tokens well over max_prompt (800)
        budget.set_history_tokens(2000);

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let result = compressor.compress(&mut history, &mut budget, 2);

        assert!(result.messages_removed > 0);
        assert!(history.len() >= 2);
        // The first remaining message should be one of the later ones
        assert!(!history[0].content.contains("message 0"));
    }

    #[test]
    fn test_min_messages_respected() {
        let mut history = make_history(3);
        let mut budget = TokenBudget::with_window("test", 1000, 200);
        budget.set_history_tokens(2000);

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let result = compressor.compress(&mut history, &mut budget, 3);

        // 3 messages with min=3 → nothing removed
        assert_eq!(result.messages_removed, 0);
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn test_skill_activation_survives_truncation() {
        let mut history = vec![
            ChatMessage::user("[ACTIVATED SKILL: \"ipo-research\"]\nThe user has indicated..."),
            ChatMessage::assistant("I'll search for data".to_owned()),
            ChatMessage::user("Search result 1...".to_owned()),
            ChatMessage::assistant("Let me search more".to_owned()),
            ChatMessage::user("Search result 2...".to_owned()),
            ChatMessage::assistant("I need more data".to_owned()),
            ChatMessage::user("Search result 3...".to_owned()),
        ];
        let mut budget = TokenBudget::with_window("test", 1000, 200);
        budget.set_history_tokens(2000);

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let result = compressor.compress(&mut history, &mut budget, 2);

        assert!(result.messages_removed > 0);
        // The skill activation message should still be present
        assert!(history.iter().any(|m| m.content.starts_with("[ACTIVATED SKILL:")));
        // At least min_messages remain
        assert!(history.len() >= 2);
    }
}
