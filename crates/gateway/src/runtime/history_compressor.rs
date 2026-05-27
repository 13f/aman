// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

#![allow(dead_code)]

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

/// Configuration values needed by the compressor (subset of the config crate type).
/// Kept as a simple struct to avoid depending on the config crate from gateway.
#[derive(Debug, Clone)]
pub struct CompressorConfig {
    pub tail_budget_ratio: f64,
    pub protect_head_messages: usize,
    pub min_tail_messages: usize,
    pub max_tool_args_chars: usize,
    pub dedup_tool_outputs: bool,
    pub summarize_tool_results: bool,
    pub truncate_tool_args: bool,
}

impl Default for CompressorConfig {
    fn default() -> Self {
        Self {
            tail_budget_ratio: 0.20,
            protect_head_messages: 2,
            min_tail_messages: 3,
            max_tool_args_chars: 500,
            dedup_tool_outputs: true,
            summarize_tool_results: true,
            truncate_tool_args: true,
        }
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
    /// This is the legacy flat-truncation path, kept for backward compat.
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
        self.truncate(history, budget, amount_to_save, min_messages)
    }

    // ── Three-segment structured compression ──

    /// Full pipeline: Stage 1 pruning (string ops) → Stage 2 boundary truncation.
    /// This is the primary entry point for Hermes-style compression.
    pub fn compress_with_boundaries(
        &self,
        history: &mut Vec<ChatMessage>,
        budget: &mut TokenBudget,
        config: &CompressorConfig,
    ) -> CompressResult {
        budget.start_compression();

        // Phase 1: Stage 1 pruning across entire history (zero API cost)
        let mut stage1_saved = 0usize;

        if config.dedup_tool_outputs {
            let (_, saved) = self.dedup_tool_outputs(history);
            stage1_saved += saved;
        }

        if config.truncate_tool_args {
            let (_, saved) = self.truncate_tool_args(history, config.max_tool_args_chars);
            stage1_saved += saved;
        }

        // Re-estimate after Stage 1
        let reestimate: usize = history
            .iter()
            .map(|m| TokenBudget::estimate_tokens(&m.content))
            .sum();
        budget.set_history_tokens(reestimate);

        // If Stage 1 was sufficient, stop here
        if !budget.needs_trim() {
            let saved = budget.pre_compression_total.saturating_sub(reestimate) + stage1_saved;
            budget.record_compression(saved);
            return CompressResult {
                messages_removed: 0,
                tokens_saved: saved,
                strategy: self.strategy,
            };
        }

        // Phase 2: Identify three segments
        let threshold_tokens =
            (budget.context_window as f64 * budget.compression_threshold) as usize;
        let mut segments = self.identify_segments(history, budget, threshold_tokens, config);
        self.align_boundaries(history, &mut segments);

        // Phase 3: Summarize tool results in MIDDLE segment only
        if config.summarize_tool_results {
            let (_, summarized) = self.summarize_tool_results(
                history,
                segments.head_len,
                segments.tail_start,
            );
            stage1_saved += summarized;
        }

        // Phase 4: Remove middle segment messages until under threshold
        let tokens_over = reestimate.saturating_sub(threshold_tokens);
        let (removed, token_savings) =
            self.remove_from_middle(history, &segments, tokens_over);

        let total_saved = stage1_saved + token_savings;
        let remaining_tokens: usize = history
            .iter()
            .map(|m| TokenBudget::estimate_tokens(&m.content))
            .sum();
        budget.set_history_tokens(remaining_tokens);
        budget.record_compression(total_saved);

        CompressResult {
            messages_removed: removed,
            tokens_saved: total_saved,
            strategy: self.strategy,
        }
    }

    // ── Stage 1: Tool Output Pruning (zero API cost) ──

    /// Replace duplicate tool outputs with a placeholder.
    /// Returns (messages_replaced, tokens_saved).
    fn dedup_tool_outputs(&self, history: &mut [ChatMessage]) -> (usize, usize) {
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut replaced = 0usize;
        let mut saved = 0usize;

        for (i, msg) in history.iter_mut().enumerate() {
            if msg.role == kernel::react::ChatMessageRole::Tool {
                let tool_name = msg.tool_name.as_deref().unwrap_or("unknown");
                let key = format!(
                    "{}|{}",
                    tool_name,
                    &msg.content[..msg.content.len().min(200)]
                );
                if let Some(&first_idx) = seen.get(&key) {
                    let original_tokens = TokenBudget::estimate_tokens(&msg.content);
                    msg.content =
                        format!("[Duplicate tool output: {tool_name} — same result as earlier call #{first_idx}]");
                    saved += original_tokens.saturating_sub(
                        TokenBudget::estimate_tokens(&msg.content),
                    );
                    replaced += 1;
                } else {
                    seen.insert(key, i);
                }
            }
        }
        (replaced, saved)
    }

    /// Truncate oversized tool_call arguments JSON.
    /// Returns (args_truncated, tokens_saved).
    fn truncate_tool_args(
        &self,
        history: &mut [ChatMessage],
        max_chars: usize,
    ) -> (usize, usize) {
        let mut truncated = 0usize;
        let mut saved = 0usize;

        for msg in history.iter_mut() {
            if msg.role != kernel::react::ChatMessageRole::Assistant {
                continue;
            }
            let Some(tool_calls) = msg.tool_calls.as_mut() else {
                continue;
            };
            for tc in tool_calls {
                let Some(function) = tc.get_mut("function") else {
                    continue;
                };
                let Some(args) = function.get_mut("arguments") else {
                    continue;
                };
                let args_str = match args {
                    serde_json::Value::String(s) => s.clone(),
                    ref other => other.to_string(),
                };
                if args_str.len() > max_chars {
                    let original_tokens = TokenBudget::estimate_tokens(&args_str);
                    let truncated_str = format!(
                        "{}…[truncated {} chars]",
                        &args_str[..max_chars.min(args_str.len())],
                        args_str.len().saturating_sub(max_chars)
                    );
                    saved += original_tokens
                        .saturating_sub(TokenBudget::estimate_tokens(&truncated_str));
                    *args = serde_json::Value::String(truncated_str);
                    truncated += 1;
                }
            }
        }
        (truncated, saved)
    }

    /// Replace verbose tool results (in MIDDLE segment) with one-line summaries.
    /// Returns (messages_summarized, tokens_saved).
    fn summarize_tool_results(
        &self,
        history: &mut [ChatMessage],
        middle_start: usize,
        tail_start: usize,
    ) -> (usize, usize) {
        let mut summarized = 0usize;
        let mut saved = 0usize;

        let end = tail_start.min(history.len());
        for msg in &mut history[middle_start..end] {
            if msg.role != kernel::react::ChatMessageRole::Tool {
                continue;
            }
            let tool_name = msg.tool_name.as_deref().unwrap_or("unknown");
            let line_count = msg.content.lines().count();
            if line_count <= 3 {
                continue; // already short
            }
            let exit_code = extract_exit_code(&msg.content);
            let first_line = msg.content.lines().next().unwrap_or("");

            let summary = format!(
                "[{tool_name}] {first_line} → exit {exit_code}, {line_count} lines output"
            );

            let original_tokens = TokenBudget::estimate_tokens(&msg.content);
            saved += original_tokens.saturating_sub(TokenBudget::estimate_tokens(&summary));
            msg.content = summary;
            summarized += 1;
        }
        (summarized, saved)
    }

    // ── Stage 2: Three-segment boundary truncation ──

    /// Identify HEAD, MIDDLE, and TAIL segment boundaries.
    fn identify_segments(
        &self,
        history: &[ChatMessage],
        _budget: &TokenBudget,
        threshold_tokens: usize,
        config: &CompressorConfig,
    ) -> ThreeSegments {
        let head_len = config.protect_head_messages.min(history.len());
        let head_tokens: usize = history[..head_len]
            .iter()
            .map(|m| TokenBudget::estimate_tokens(&m.content))
            .sum();

        let max_tail_tokens =
            (threshold_tokens as f64 * config.tail_budget_ratio) as usize;

        // Walk backwards to build tail
        let mut tail_start = history.len();
        let mut tail_tokens = 0usize;
        let mut found_latest_user = false;

        for (collected, i) in (head_len..history.len()).rev().enumerate() {
            let msg_tokens = TokenBudget::estimate_tokens(&history[i].content);
            let would_exceed = tail_tokens + msg_tokens > max_tail_tokens;

            if history[i].role == kernel::react::ChatMessageRole::User {
                found_latest_user = true;
            }

            // Stop when we already have enough messages AND adding one more would exceed budget
            if collected >= config.min_tail_messages && would_exceed {
                break;
            }

            tail_start = i;
            tail_tokens += msg_tokens;
        }

        // Ensure latest user message is in tail
        if !found_latest_user {
            for i in (head_len..tail_start).rev() {
                tail_start = i;
                tail_tokens += TokenBudget::estimate_tokens(&history[i].content);
                if history[i].role == kernel::react::ChatMessageRole::User {
                    break;
                }
            }
        }

        let middle_tokens: usize = history[head_len..tail_start]
            .iter()
            .map(|m| TokenBudget::estimate_tokens(&m.content))
            .sum();

        ThreeSegments {
            head_len,
            tail_start,
            tail_tokens,
            head_tokens,
            middle_tokens,
        }
    }

    /// Ensure boundaries never split tool_call/tool_result pairs.
    fn align_boundaries(
        &self,
        history: &[ChatMessage],
        segments: &mut ThreeSegments,
    ) {
        // If TAIL starts with a Tool message whose matching tool_call is in MIDDLE,
        // pull the tool_call (and its assistant message) into TAIL.
        if segments.tail_start < history.len() {
            let first_tail = &history[segments.tail_start];
            if first_tail.role == kernel::react::ChatMessageRole::Tool
                && let Some(call_id) = &first_tail.tool_call_id {
                    // Find the assistant message with this tool_call in MIDDLE
                    for i in (segments.head_len..segments.tail_start).rev() {
                        if history[i].role == kernel::react::ChatMessageRole::Assistant
                            && let Some(tcs) = &history[i].tool_calls {
                                let has_match = tcs.iter().any(|tc| {
                                    tc.get("id")
                                        .and_then(|v| v.as_str())
                                        .is_some_and(|id| id == *call_id)
                                });
                                if has_match {
                                    segments.tail_start = i;
                                    // Recalculate tokens (approximate)
                                    break;
                                }
                            }
                    }
                }
        }
    }

    /// Remove messages from the MIDDLE segment until we free enough tokens.
    /// Removes oldest messages first, preferring tool-result pairs.
    fn remove_from_middle(
        &self,
        history: &mut Vec<ChatMessage>,
        segments: &ThreeSegments,
        target_tokens: usize,
    ) -> (usize, usize) {
        let middle_len = segments.tail_start.saturating_sub(segments.head_len);
        if middle_len == 0 {
            return (0, 0);
        }

        // Remove from the middle start, protecting at least 1 message as bridge
        let remove_count = if middle_len > 2 {
            middle_len / 2
        } else if middle_len > 1 {
            1
        } else {
            0
        };

        if remove_count == 0 {
            return (0, 0);
        }

        let removed_msgs: Vec<ChatMessage> =
            history.drain(segments.head_len..segments.head_len + remove_count).collect();
        let mut removed = removed_msgs.len();
        let mut saved: usize = removed_msgs
            .iter()
            .map(|m| TokenBudget::estimate_tokens(&m.content))
            .sum();

        // If still over target, remove more
        let current: usize = history
            .iter()
            .map(|m| TokenBudget::estimate_tokens(&m.content))
            .sum();
        let over = current.saturating_sub(target_tokens);

        if over > 0 && removed < segments.tail_start.saturating_sub(segments.head_len) {
            // Remove additional individual messages from the (now shifted) middle
            let new_middle_start = segments.head_len;
            let new_middle_end = segments.tail_start - removed;
            let available = new_middle_end.saturating_sub(new_middle_start);
            if available > 1 {
                let extra = available / 2;
                let extra_msgs: Vec<ChatMessage> =
                    history.drain(new_middle_start..new_middle_start + extra).collect();
                let extra_saved: usize = extra_msgs
                    .iter()
                    .map(|m| TokenBudget::estimate_tokens(&m.content))
                    .sum();
                removed += extra_msgs.len();
                saved += extra_saved;
            }
        }

        (removed, saved)
    }

    // ── Legacy truncation (kept for backward compat) ──

    /// Remove oldest messages until the target token savings are achieved
    /// and at least `min_messages` remain.
    fn truncate(
        &self,
        history: &mut Vec<ChatMessage>,
        budget: &mut TokenBudget,
        target_save: usize,
        min_messages: usize,
    ) -> CompressResult {
        let mut removed = 0usize;
        let mut tokens_saved = 0usize;

        while history.len() > min_messages && tokens_saved < target_save {
            let is_protected = is_skill_activation(&history[0].content);
            if is_protected {
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

/// Identified segment boundaries for three-segment compression.
#[derive(Debug, Clone)]
struct ThreeSegments {
    /// Index where HEAD ends and MIDDLE begins.
    head_len: usize,
    /// Index where MIDDLE ends and TAIL begins.
    tail_start: usize,
    head_tokens: usize,
    middle_tokens: usize,
    tail_tokens: usize,
}

/// Result of a compression operation.
#[derive(Debug, Clone)]
pub struct CompressResult {
    pub messages_removed: usize,
    pub tokens_saved: usize,
    pub strategy: CompressionStrategy,
}

/// Check if a message contains skill activation content that should survive truncation.
fn is_skill_activation(content: &str) -> bool {
    content.starts_with("[ACTIVATED SKILL:")
        || content.starts_with("[The skill \"")
        || content.starts_with("[FORMAT INSTRUCTION]")
}

/// Try to extract an exit code from tool output text.
fn extract_exit_code(content: &str) -> String {
    // Common patterns: "exit code: 0", "Exited with code 1", "exit status: 0"
    for line in content.lines().rev() {
        let lower = line.to_lowercase();
        if let Some(pos) = lower.find("exit")
            && let Some(code_start) = lower[pos..].find(|c: char| c.is_ascii_digit())
            {
                let code = &lower[pos + code_start..];
                let num: String = code.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !num.is_empty() {
                    return num;
                }
            }
        // "Process exited with code 0"
        if let Some(pos) = lower.find("code")
            && let Some(code_start) = lower[pos..].find(|c: char| c.is_ascii_digit())
            {
                let code = &lower[pos + code_start..];
                let num: String = code.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !num.is_empty() {
                    return num;
                }
            }
    }
    "?".to_string()
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

    fn make_tool_pair(user_idx: usize, tool_name: &str, content: &str, call_id: &str) -> [ChatMessage; 2] {
        use serde_json::json;
        let assistant = ChatMessage {
            role: kernel::react::ChatMessageRole::Assistant,
            content: format!("calling {tool_name}"),
            tool_call_id: None,
            tool_name: None,
            tool_calls: Some(vec![json!({
                "id": call_id,
                "function": {
                    "name": tool_name,
                    "arguments": "{}"
                }
            })]),
            reasoning_content: String::new(),
        };
        let tool_result = ChatMessage {
            role: kernel::react::ChatMessageRole::Tool,
            content: content.to_string(),
            tool_call_id: Some(call_id.to_string()),
            tool_name: Some(tool_name.to_string()),
            tool_calls: None,
            reasoning_content: String::new(),
        };
        [assistant, tool_result]
    }

    #[test]
    fn test_no_compression_when_under_budget() {
        let mut history = make_history(4);
        let mut budget = TokenBudget::with_window("test", 1000, 200);
        budget.set_history_tokens(100);

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let result = compressor.compress(&mut history, &mut budget, 2);

        assert_eq!(result.messages_removed, 0);
        assert_eq!(history.len(), 4);
    }

    #[test]
    fn test_truncate_removes_oldest() {
        let mut history = make_history(10);
        let mut budget = TokenBudget::with_window("test", 1000, 200);
        budget.set_history_tokens(2000);

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let result = compressor.compress(&mut history, &mut budget, 2);

        assert!(result.messages_removed > 0);
        assert!(history.len() >= 2);
        assert!(!history[0].content.contains("message 0"));
    }

    #[test]
    fn test_min_messages_respected() {
        let mut history = make_history(3);
        let mut budget = TokenBudget::with_window("test", 1000, 200);
        budget.set_history_tokens(2000);

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let result = compressor.compress(&mut history, &mut budget, 3);

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
        assert!(history.iter().any(|m| m.content.starts_with("[ACTIVATED SKILL:")));
        assert!(history.len() >= 2);
    }

    // ── Stage 1 tests ──

    #[test]
    fn test_dedup_identical_tool_outputs() {
        let long_content = "x".repeat(500);
        let [a1, t1] = make_tool_pair(0, "read", &long_content, "call_1");
        let [a2, t2] = make_tool_pair(1, "read", &long_content, "call_2");
        let mut history = vec![
            ChatMessage::user("read the file"),
            a1, t1,
            ChatMessage::user("read it again"),
            a2, t2,
        ];

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let (replaced, saved) = compressor.dedup_tool_outputs(&mut history);

        assert_eq!(replaced, 1);
        assert!(saved > 0);
        // The second tool result is at index 5 (user at 3, assistant at 4, tool at 5)
        assert!(history[5].content.starts_with("[Duplicate tool output:"));
    }

    #[test]
    fn test_dedup_different_outputs_preserved() {
        let [a1, t1] = make_tool_pair(0, "read", "file A contents", "call_1");
        let [a2, t2] = make_tool_pair(1, "read", "file B contents", "call_2");
        let mut history = vec![
            ChatMessage::user("read"),
            a1, t1, a2, t2,
        ];

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let (replaced, _) = compressor.dedup_tool_outputs(&mut history);

        assert_eq!(replaced, 0);
    }

    #[test]
    fn test_truncate_large_tool_args() {
        use serde_json::json;
        let long_args = "x".repeat(600);
        let assistant = ChatMessage {
            role: kernel::react::ChatMessageRole::Assistant,
            content: "calling tool".to_string(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: Some(vec![json!({
                "id": "call_1",
                "function": {
                    "name": "search",
                    "arguments": long_args
                }
            })]),
            reasoning_content: String::new(),
        };
        let mut history = vec![ChatMessage::user("search"), assistant];

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let (truncated, saved) = compressor.truncate_tool_args(&mut history, 500);

        assert_eq!(truncated, 1);
        assert!(saved > 0);
        // Verify the args were truncated
        if let Some(tcs) = &history[1].tool_calls {
            let args = tcs[0]["function"]["arguments"].as_str().unwrap();
            assert!(args.contains("[truncated"));
        }
    }

    #[test]
    fn test_truncate_under_limit() {
        use serde_json::json;
        let assistant = ChatMessage {
            role: kernel::react::ChatMessageRole::Assistant,
            content: "calling tool".to_string(),
            tool_call_id: None,
            tool_name: None,
            tool_calls: Some(vec![json!({
                "id": "call_1",
                "function": {
                    "name": "search",
                    "arguments": "short arg"
                }
            })]),
            reasoning_content: String::new(),
        };
        let mut history = vec![ChatMessage::user("search"), assistant];

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let (truncated, _) = compressor.truncate_tool_args(&mut history, 500);

        assert_eq!(truncated, 0);
    }

    #[test]
    fn test_summarize_long_tool_output() {
        // Build content with many lines so it triggers summarization
        let mut content = String::new();
        for i in 0..50 {
            content.push_str(&format!("line {i}\n"));
        }
        content.push_str("Process exited with code 0\n");
        let [a1, t1] = make_tool_pair(0, "terminal", &content, "call_1");
        let mut history = vec![
            ChatMessage::user("run command"),
            a1, t1,
        ];

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let (summarized, saved) = compressor.summarize_tool_results(&mut history, 0, 3);

        assert_eq!(summarized, 1, "should have summarized 1 tool result");
        assert!(saved > 0);
        assert!(history[2].content.contains("→ exit 0"),
            "summary should contain exit code: {}", history[2].content);
        assert!(history[2].content.contains("lines output"));
    }

    #[test]
    fn test_summarize_skips_tail() {
        let [a1, t1] = make_tool_pair(0, "terminal",
            &("line ".to_string().repeat(100)),
            "call_1");
        let original = t1.content.clone();
        let mut history = vec![
            ChatMessage::user("run"),
            a1, t1,
        ];

        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        // tail_start = 2 → skips the tool result at index 2
        let (summarized, _) = compressor.summarize_tool_results(&mut history, 0, 2);

        assert_eq!(summarized, 0);
        assert_eq!(history[2].content, original);
    }

    // ── Three-segment tests ──

    #[test]
    fn test_head_protected() {
        let history = make_history(12);
        let mut budget = TokenBudget::with_window("test", 10_000, 0);
        budget.current_history_tokens = 5000;
        budget.set_history_tokens(5000);

        let threshold_tokens = (budget.context_window as f64 * budget.compression_threshold) as usize;
        let config = CompressorConfig {
            protect_head_messages: 2,
            ..Default::default()
        };
        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let segments = compressor.identify_segments(&history, &budget, threshold_tokens, &config);

        assert_eq!(segments.head_len, 2);
        // The first 2 messages should survive
    }

    #[test]
    fn test_tail_protected() {
        // Build history with large messages so tail budget kicks in
        let padding = "x".repeat(3000); // ~1000 estimated tokens per message
        let history: Vec<ChatMessage> = (0..12)
            .map(|i| {
                if i % 2 == 0 {
                    ChatMessage::user(format!("user {i}: {padding}"))
                } else {
                    ChatMessage::assistant(format!("reply {i}: {padding}"))
                }
            })
            .collect();

        let mut budget = TokenBudget::with_window("test", 5000, 0);
        budget.compression_threshold = 0.80;
        let total: usize = history.iter()
            .map(|m| TokenBudget::estimate_tokens(&m.content))
            .sum();
        budget.set_history_tokens(total);

        let threshold_tokens = (budget.context_window as f64 * budget.compression_threshold) as usize;
        let config = CompressorConfig::default();
        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let segments = compressor.identify_segments(&history, &budget, threshold_tokens, &config);

        // Tail should include the last few messages
        assert!(segments.tail_start < history.len());
        assert!(segments.tail_start > segments.head_len,
            "tail_start={} should be > head_len={}", segments.tail_start, segments.head_len);
        assert!(history.len() - segments.tail_start >= config.min_tail_messages);
    }

    #[test]
    fn test_tool_pair_not_split() {
        let [a1, t1] = make_tool_pair(0, "read", "contents", "call_1");
        let history = vec![
            ChatMessage::user("msg 0"),
            ChatMessage::assistant("reply 1".to_owned()),
            ChatMessage::user("msg 2"),
            ChatMessage::assistant("reply 3".to_owned()),
            ChatMessage::user("msg 4"),
            a1, t1,  // tool pair at indices 6, 7
            ChatMessage::user("final user msg 8"),
        ];

        let mut budget = TokenBudget::with_window("test", 10_000, 0);
        budget.current_history_tokens = 3000;
        budget.set_history_tokens(3000);

        let threshold_tokens = (budget.context_window as f64 * budget.compression_threshold) as usize;
        let config = CompressorConfig {
            protect_head_messages: 2,
            min_tail_messages: 3,
            ..Default::default()
        };
        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let mut segments = compressor.identify_segments(&history, &budget, threshold_tokens, &config);

        let original_tail_start = segments.tail_start;
        compressor.align_boundaries(&history, &mut segments);

        // If the tool result at index 7 was the first in tail (tail_start=7),
        // it should have been adjusted to include the assistant at index 6.
        if original_tail_start == 7 {
            assert!(segments.tail_start <= 6);
        }
    }

    #[test]
    fn test_full_compression_pipeline() {
        let mut history = make_history(10);
        // Make the history very large in estimated tokens
        for msg in &mut history {
            msg.content = format!("{} {}", msg.content, "padding ".repeat(500));
        }

        let mut budget = TokenBudget::with_window("test", 5000, 500);
        // Set history tokens way above threshold
        let total: usize = history
            .iter()
            .map(|m| TokenBudget::estimate_tokens(&m.content))
            .sum();
        budget.set_history_tokens(total);

        assert!(budget.needs_trim());

        let config = CompressorConfig::default();
        let compressor = HistoryCompressor::new(CompressionStrategy::Truncate);
        let result = compressor.compress_with_boundaries(&mut history, &mut budget, &config);

        assert!(result.tokens_saved > 0);
        // After compression, should not need trimming (or at least have fewer tokens)
        assert!(history.len() < 10 || !budget.needs_trim());
    }
}
