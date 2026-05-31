// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::react::{ChatMessage, ChatMessageRole, ParsedToolCall};

/// Priority score for a context item (0.0–1.0).
///
/// Used to decide which messages to evict first when the token
/// budget is exceeded. Higher scores are retained; lower scores
/// are evicted.
#[derive(Debug, Clone, Copy)]
pub struct ContextPriority {
    /// Semantic relevance to the current task (0.0–1.0).
    /// Tool outputs that match the current task score higher.
    pub relevance: f64,
    /// How recently the message was added (1.0 = just added, decays to 0.0).
    /// Calculated as 1.0 - (position_from_end / total_messages).
    pub recency: f64,
    /// Intrinsic importance (1.0 = system prompt, 0.8 = task description, 0.3 = regular message).
    pub importance: f64,
    /// Observed utility — did this message lead to a useful outcome?
    /// Tool calls that returned useful data or led to task progress score higher.
    pub utility: f64,
}

impl ContextPriority {
    /// Composite score: weighted average of all dimensions.
    pub fn score(&self) -> f64 {
        // Weights: relevance 0.30, recency 0.25, importance 0.25, utility 0.20
        self.relevance * 0.30 + self.recency * 0.25 + self.importance * 0.25 + self.utility * 0.20
    }

    /// Minimum viable score — items below this are eviction candidates.
    pub const EVICTION_THRESHOLD: f64 = 0.15;
}

impl Default for ContextPriority {
    fn default() -> Self {
        Self {
            relevance: 0.3,
            recency: 1.0,
            importance: 0.3,
            utility: 0.0,
        }
    }
}

/// Scores context items for priority-based eviction.
///
/// Tracks observed utility of tool calls across turns and
/// assigns priority scores to each message in the history.
pub struct PriorityScorer {
    /// Tool names that produced high-utility outputs (exit code 0, valid data).
    useful_tools: Vec<String>,
    /// Recently referenced entities / keywords from user messages.
    active_topics: Vec<String>,
}

impl PriorityScorer {
    pub fn new() -> Self {
        Self {
            useful_tools: Vec::new(),
            active_topics: Vec::new(),
        }
    }

    /// Record the outcome of a turn for utility tracking.
    pub fn record_turn(&mut self, content: &str, tool_calls: &[ParsedToolCall]) {
        // Track which tools were called
        for tc in tool_calls {
            if !self.useful_tools.contains(&tc.tool_name) {
                self.useful_tools.push(tc.tool_name.clone());
            }
        }

        // Extract potential topics from the response (simple keyword extraction)
        for word in content.split_whitespace() {
            let cleaned = word.trim_matches(|c: char| !c.is_alphanumeric());
            if cleaned.len() > 4 && !self.active_topics.contains(&cleaned.to_lowercase()) {
                self.active_topics.push(cleaned.to_lowercase());
                if self.active_topics.len() > 20 {
                    self.active_topics.remove(0);
                }
            }
        }
    }

    /// Score a single message for eviction priority.
    ///
    /// Higher score = more important = keep longer.
    pub fn score_message(&self, msg: &ChatMessage, index: usize, total: usize) -> ContextPriority {
        let recency = if total > 1 {
            1.0 - (index as f64 / (total - 1) as f64)
        } else {
            1.0
        };

        let importance = match msg.role {
            ChatMessageRole::System => 1.0,
            ChatMessageRole::User => 0.7,
            ChatMessageRole::Assistant => 0.5,
            ChatMessageRole::Tool => 0.4,
        };

        // Relevance: does this message mention active topics or useful tools?
        let relevance = self.compute_relevance(msg);

        // Utility: tool results from known-useful tools get a boost
        let utility = if msg.role == ChatMessageRole::Tool {
            if let Some(tool_name) = &msg.tool_name {
                if self.useful_tools.contains(tool_name) {
                    0.7
                } else {
                    0.2
                }
            } else {
                0.2
            }
        } else {
            0.0
        };

        ContextPriority {
            relevance,
            recency,
            importance,
            utility,
        }
    }

    /// Compute relevance of a message to active topics.
    fn compute_relevance(&self, msg: &ChatMessage) -> f64 {
        if self.active_topics.is_empty() {
            return 0.3; // neutral
        }

        let content_lower = msg.content.to_lowercase();
        let mut matches = 0usize;

        for topic in &self.active_topics {
            if content_lower.contains(topic.as_str()) {
                matches += 1;
            }
        }

        if matches == 0 {
            0.2
        } else if matches >= 3 {
            0.9
        } else {
            0.4 + (matches as f64 * 0.15)
        }
    }

    /// Rank messages by priority and return indices to evict.
    ///
    /// Returns a sorted list of indices (lowest priority first) for the
    /// given number of messages to remove.
    pub fn select_eviction_candidates(
        &self,
        messages: &[ChatMessage],
        count: usize,
    ) -> Vec<usize> {
        let total = messages.len();
        let mut scored: Vec<(usize, f64)> = messages
            .iter()
            .enumerate()
            .map(|(i, msg)| {
                let priority = self.score_message(msg, i, total);
                (i, priority.score())
            })
            .collect();

        // Sort by score ascending (lowest first = eviction candidates)
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        scored
            .into_iter()
            .take(count)
            .map(|(i, _)| i)
            .collect()
    }
}

impl Default for PriorityScorer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_priority_score_range() {
        let p = ContextPriority {
            relevance: 0.5,
            recency: 0.5,
            importance: 0.5,
            utility: 0.5,
        };
        let score = p.score();
        assert!(score > 0.0 && score <= 1.0);

        let p_max = ContextPriority {
            relevance: 1.0,
            recency: 1.0,
            importance: 1.0,
            utility: 1.0,
        };
        assert!((p_max.score() - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_eviction_candidates_sorted() {
        let scorer = PriorityScorer::new();
        let messages = vec![
            ChatMessage::user("hello world"),
            ChatMessage::assistant("hi there"),
            ChatMessage::user("do something"),
            ChatMessage::assistant("ok done"),
        ];

        let candidates = scorer.select_eviction_candidates(&messages, 2);
        assert_eq!(candidates.len(), 2);
        // First candidate should have lower priority than remaining messages
        let all_scores: Vec<f64> = messages
            .iter()
            .enumerate()
            .map(|(i, msg)| scorer.score_message(msg, i, messages.len()).score())
            .collect();

        // The two candidates should be among the lowest-scored
        let candidate_scores: Vec<f64> = candidates.iter().map(|&i| all_scores[i]).collect();
        let remaining_scores: Vec<f64> = (0..messages.len())
            .filter(|i| !candidates.contains(i))
            .map(|i| all_scores[i])
            .collect();

        for cs in &candidate_scores {
            for rs in &remaining_scores {
                assert!(cs <= rs, "candidate score {cs} should be <= remaining score {rs}");
            }
        }
    }

    #[test]
    fn test_user_messages_higher_priority_than_tool() {
        let scorer = PriorityScorer::new();
        let user_msg = ChatMessage::user("important task");
        let tool_msg = ChatMessage {
            role: ChatMessageRole::Tool,
            content: "tool output".to_string(),
            tool_call_id: Some("call_1".to_string()),
            tool_name: Some("unknown_tool".to_string()),
            tool_calls: None,
            reasoning_content: String::new(),
        };

        let user_score = scorer.score_message(&user_msg, 0, 2).score();
        let tool_score = scorer.score_message(&tool_msg, 1, 2).score();

        assert!(user_score > tool_score,
            "user msg ({user_score}) should outrank tool msg ({tool_score})");
    }
}
