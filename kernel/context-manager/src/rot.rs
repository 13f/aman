// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use kernel::react::{ChatMessage, ChatMessageRole, ParsedToolCall};
use std::collections::HashMap;

/// A signal that the context window may be degrading ("context rot").
///
/// When these signals accumulate, the context manager can take
/// corrective action — clearing stale tool results, re-injecting
/// the task description, or triggering a summary compression.
#[derive(Debug, Clone, PartialEq)]
pub enum RotSignal {
    /// The same tool is being called with the same (or very similar) arguments
    /// repeatedly, suggesting the agent forgot earlier results.
    RepeatedToolCall {
        tool_name: String,
        /// How many times this pattern was observed.
        count: usize,
    },
    /// The agent's output contradicts something it said earlier.
    Contradiction {
        /// Brief description of the contradiction.
        description: String,
    },
    /// The agent's responses are drifting away from the task.
    OffTopicDrift {
        /// The original task keywords that are no longer referenced.
        missing_topics: Vec<String>,
    },
    /// The agent referenced a file path, tool name, or entity that doesn't exist.
    Hallucination {
        /// What was fabricated.
        fabricated: String,
    },
    /// Many consecutive tool results with non-zero exit codes,
    /// suggesting the agent is stuck in an error loop.
    ToolErrorLoop {
        /// Number of consecutive failed tool calls.
        consecutive_failures: usize,
    },
}

impl RotSignal {
    /// Severity level: 1 (warning) to 3 (critical).
    pub fn severity(&self) -> u8 {
        match self {
            Self::RepeatedToolCall { count, .. } => {
                if *count >= 5 {
                    3
                } else if *count >= 3 {
                    2
                } else {
                    1
                }
            }
            Self::Contradiction { .. } => 2,
            Self::OffTopicDrift { .. } => 2,
            Self::Hallucination { .. } => 1,
            Self::ToolErrorLoop { consecutive_failures } => {
                if *consecutive_failures >= 5 {
                    3
                } else if *consecutive_failures >= 3 {
                    2
                } else {
                    1
                }
            }
        }
    }

    /// Human-readable label for the signal type.
    pub fn label(&self) -> &str {
        match self {
            Self::RepeatedToolCall { .. } => "repeated_tool_call",
            Self::Contradiction { .. } => "contradiction",
            Self::OffTopicDrift { .. } => "off_topic_drift",
            Self::Hallucination { .. } => "hallucination",
            Self::ToolErrorLoop { .. } => "tool_error_loop",
        }
    }
}

/// Detects context rot by monitoring patterns across ReAct turns.
///
/// Maintains a sliding window of recent tool calls and their outcomes,
/// and checks for common degradation patterns after each turn.
pub struct RotDetector {
    /// Recent tool calls: (tool_name, args_snippet) → count.
    recent_tool_calls: HashMap<String, usize>,
    /// Recent tool results: tool_call_id → exit_code_success.
    recent_tool_results: Vec<bool>,
    /// Task keywords extracted from the initial user message.
    task_keywords: Vec<String>,
    /// Number of consecutive failed tool calls.
    consecutive_failures: usize,
    /// Recent assistant responses for contradiction checking (last 5).
    recent_responses: Vec<String>,
    /// Turn counter.
    turn: u32,
}

impl RotDetector {
    pub fn new() -> Self {
        Self {
            recent_tool_calls: HashMap::new(),
            recent_tool_results: Vec::new(),
            task_keywords: Vec::new(),
            consecutive_failures: 0,
            recent_responses: Vec::new(),
            turn: 0,
        }
    }

    /// Set task keywords from the initial user message.
    /// Call once at session start.
    pub fn set_task_keywords(&mut self, user_text: &str) {
        self.task_keywords = user_text
            .split_whitespace()
            .filter(|w| w.len() > 3)
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
            .filter(|w| !is_stop_word(w))
            .collect();
    }

    /// Detect rot signals in the current conversation history.
    ///
    /// Called before each ReAct turn to check for degradation.
    pub fn detect(&self, history: &[ChatMessage], turn: u32) -> Vec<RotSignal> {
        if turn < 3 {
            return Vec::new(); // Not enough history for meaningful detection
        }

        let mut signals = Vec::new();

        // Check for repeated tool calls
        if let Some(signal) = self.detect_repeated_tool_calls(history) {
            signals.push(signal);
        }

        // Check for off-topic drift
        if let Some(signal) = self.detect_off_topic_drift(history) {
            signals.push(signal);
        }

        // Check for tool error loops
        if let Some(signal) = self.detect_tool_error_loop() {
            signals.push(signal);
        }

        signals
    }

    /// Feed the outcome of a completed turn into the detector.
    pub fn feed_turn(&mut self, content: &str, tool_calls: &[ParsedToolCall]) {
        self.turn += 1;

        // Track tool calls for repetition detection
        for tc in tool_calls {
            let args_str = tc.args.to_string();
            let args_snippet = &args_str[..args_str.len().min(100)];
            let key = format!("{}|{}", tc.tool_name, args_snippet);
            *self.recent_tool_calls.entry(key).or_insert(0) += 1;
        }

        // Track assistant responses for contradiction detection
        if !content.is_empty() {
            self.recent_responses.push(content.to_string());
            if self.recent_responses.len() > 5 {
                self.recent_responses.remove(0);
            }
        }

        // Periodically prune old entries from the tool call map
        if self.turn.is_multiple_of(10) {
            self.recent_tool_calls.retain(|_, count| {
                *count > 1 // Keep only repeated calls for detection
            });
        }
    }

    /// Record a tool result for error-loop detection.
    pub fn record_tool_result(&mut self, success: bool) {
        self.recent_tool_results.push(success);
        if self.recent_tool_results.len() > 20 {
            self.recent_tool_results.remove(0);
        }

        if success {
            self.consecutive_failures = 0;
        } else {
            self.consecutive_failures += 1;
        }
    }

    // ── Private detectors ──

    fn detect_repeated_tool_calls(&self, _history: &[ChatMessage]) -> Option<RotSignal> {
        let mut max_repeats: Option<(&str, usize)> = None;

        for (key, &count) in &self.recent_tool_calls {
            if count >= 3 {
                let tool_name = key.split('|').next().unwrap_or(key);
                match max_repeats {
                    Some((_, existing)) if count > existing => {
                        max_repeats = Some((tool_name, count));
                    }
                    None => {
                        max_repeats = Some((tool_name, count));
                    }
                    _ => {}
                }
            }
        }

        max_repeats.map(|(tool_name, count)| RotSignal::RepeatedToolCall {
            tool_name: tool_name.to_string(),
            count,
        })
    }

    fn detect_off_topic_drift(&self, history: &[ChatMessage]) -> Option<RotSignal> {
        if self.task_keywords.is_empty() {
            return None;
        }

        // Check the last 3 assistant responses for topic relevance
        let recent: Vec<&ChatMessage> = history
            .iter()
            .rev()
            .filter(|m| m.role == ChatMessageRole::Assistant)
            .take(3)
            .collect();

        if recent.len() < 2 {
            return None;
        }

        let mut missing_topics = Vec::new();
        for keyword in &self.task_keywords {
            let found = recent
                .iter()
                .any(|m| m.content.to_lowercase().contains(keyword.as_str()));
            if !found {
                missing_topics.push(keyword.clone());
            }
        }

        // Signal drift if > 50% of task keywords are missing from recent responses
        if missing_topics.len() > self.task_keywords.len() / 2 {
            Some(RotSignal::OffTopicDrift { missing_topics })
        } else {
            None
        }
    }

    fn detect_tool_error_loop(&self) -> Option<RotSignal> {
        if self.consecutive_failures >= 3 {
            Some(RotSignal::ToolErrorLoop {
                consecutive_failures: self.consecutive_failures,
            })
        } else {
            None
        }
    }
}

impl Default for RotDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Common stop words to exclude from keyword extraction.
fn is_stop_word(word: &str) -> bool {
    matches!(
        word,
        "the" | "and"
            | "for"
            | "that"
            | "this"
            | "with"
            | "from"
            | "have"
            | "what"
            | "when"
            | "where"
            | "which"
            | "about"
            | "than"
            | "just"
            | "like"
            | "some"
            | "also"
            | "into"
            | "more"
            | "your"
            | "them"
            | "will"
            | "can"
            | "its"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rot_signal_severity() {
        let s1 = RotSignal::RepeatedToolCall {
            tool_name: "read".to_string(),
            count: 2,
        };
        assert_eq!(s1.severity(), 1);

        let s3 = RotSignal::RepeatedToolCall {
            tool_name: "read".to_string(),
            count: 5,
        };
        assert_eq!(s3.severity(), 3);

        let err = RotSignal::ToolErrorLoop {
            consecutive_failures: 6,
        };
        assert_eq!(err.severity(), 3);
    }

    #[test]
    fn test_no_signals_early_turns() {
        let detector = RotDetector::new();
        let history = vec![
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi"),
        ];
        let signals = detector.detect(&history, 1);
        assert!(signals.is_empty());
    }

    #[test]
    fn test_detect_tool_error_loop() {
        let mut detector = RotDetector::new();
        detector.consecutive_failures = 4;
        let signals = detector.detect(&[], 5);
        assert!(!signals.is_empty());
        assert!(matches!(signals[0], RotSignal::ToolErrorLoop { .. }));
    }

    #[test]
    fn test_off_topic_drift_detected() {
        let mut detector = RotDetector::new();
        detector.set_task_keywords("build a rust web server");

        let history = vec![
            ChatMessage::user("build a rust web server"),
            ChatMessage::assistant("I'll help with the web server"),
            ChatMessage::user("what about python"),
            ChatMessage::assistant("Python is great for data science"),
            ChatMessage::user("tell me about pandas"),
            ChatMessage::assistant("Pandas is a data analysis library"),
        ];

        let signals = detector.detect(&history, 5);
        // Should detect drift away from "rust web server" topics
        let has_drift = signals.iter().any(|s| matches!(s, RotSignal::OffTopicDrift { .. }));
        assert!(has_drift, "should detect off-topic drift, got: {signals:?}");
    }

    #[test]
    fn test_feed_turn_tracks_repeated_calls() {
        let mut detector = RotDetector::new();

        let call = ParsedToolCall {
            id: "call_1".to_string(),
            tool_name: "read".to_string(),
            args: serde_json::json!({"path": "/tmp/test.txt"}),
        };

        detector.feed_turn("", std::slice::from_ref(&call));
        detector.feed_turn("", std::slice::from_ref(&call));
        detector.feed_turn("", std::slice::from_ref(&call));

        // The "read|args" key should have count 3
        assert!(!detector.recent_tool_calls.is_empty());
        let max_count = detector.recent_tool_calls.values().max().copied().unwrap_or(0);
        assert_eq!(max_count, 3);
    }

    #[test]
    fn test_record_tool_result_resets_on_success() {
        let mut detector = RotDetector::new();
        detector.record_tool_result(false);
        detector.record_tool_result(false);
        assert_eq!(detector.consecutive_failures, 2);
        detector.record_tool_result(true);
        assert_eq!(detector.consecutive_failures, 0);
    }
}
