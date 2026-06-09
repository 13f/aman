// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Session progress evaluation — lightweight heuristics to assess whether
//! a background session is making progress, stuck, or done.
//!
//! No LLM call needed. Extracts signals from conversation history text.

use serde::Serialize;
use std::collections::BTreeSet;

/// Summary of session progress extracted from conversation history.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionProgress {
    /// A collision (or other success criterion) has been found.
    pub collision_found: bool,
    /// Best partial match detected (e.g. "2/4 outputs match").
    pub best_partial_match: u32,
    /// Whether the agent appears to be in a repetitive loop.
    pub looks_stuck: bool,
    /// Distinct tools/capabilities used (proxy for strategy diversity).
    pub unique_tools: Vec<String>,
}

/// Evaluate session progress from a list of (role, content) messages.
///
/// The `messages` slice should contain the full conversation history,
/// ordered oldest-first. Only the most recent ~64 messages are examined.
pub fn evaluate(messages: &[(String, String)]) -> SessionProgress {
    let mut progress = SessionProgress::default();

    // Scan last N assistant messages for key signals
    let recent: Vec<&str> = messages
        .iter()
        .rev()
        .take(64)
        .map(|(_, content)| content.as_str())
        .collect();

    let full_text = recent.join(" ").to_lowercase();

    // ── Collision / success detection ──────────────────────────────
    if full_text.contains("collision found")
        || full_text.contains("✅ collision")
        || full_text.contains("verify_collision_solution")
    {
        progress.collision_found = true;
    }

    // ── Partial match tracking ────────────────────────────────────
    for line in &recent {
        let lower = line.to_lowercase();
        for prefix in ["best_match=", "match=", "best partial match:"] {
            if let Some(pos) = lower.find(prefix) {
                let rest = &lower[pos + prefix.len()..];
                if let Some(slash) = rest.find('/') {
                    if let Ok(n) = rest[..slash].trim().parse::<u32>() {
                        progress.best_partial_match = progress.best_partial_match.max(n);
                    }
                } else if let Ok(n) = rest
                    .trim()
                    .trim_end_matches(|c: char| !c.is_ascii_digit())
                    .parse::<u32>()
                {
                    progress.best_partial_match = progress.best_partial_match.max(n);
                }
            }
        }
    }

    // ── Tool diversity ────────────────────────────────────────────
    let known_tools = [
        "read_file", "write_file", "edit", "execute", "bash", "run",
        "search", "grep", "python", "sage", "sagemath", "groebner",
        "resultant", "z3", "newton", "smt", "cargo", "git",
    ];
    for (role, content) in messages.iter().rev().take(64) {
        if role == "assistant" {
            for tool in known_tools {
                if content.contains(tool) {
                    progress.unique_tools.push(tool.to_string());
                }
            }
        }
    }
    progress.unique_tools.sort();
    progress.unique_tools.dedup();

    // ── Stuck detection ───────────────────────────────────────────
    let assistant_msgs: Vec<&str> = messages
        .iter()
        .rev()
        .filter(|(role, _)| role == "assistant")
        .take(8)
        .map(|(_, content)| content.as_str())
        .collect();

    if assistant_msgs.len() >= 4 {
        let similar_pairs = assistant_msgs
            .windows(2)
            .filter(|w| word_overlap(w[0], w[1]) > 0.8)
            .count();
        if similar_pairs >= 3 {
            progress.looks_stuck = true;
        }
    }

    // Long session + zero partial progress → stuck
    if messages.len() > 100 && progress.best_partial_match == 0 && !progress.collision_found {
        progress.looks_stuck = true;
    }

    progress
}

/// Word overlap ratio (Jaccard similarity) between two strings.
fn word_overlap(a: &str, b: &str) -> f64 {
    let wa: BTreeSet<&str> = a.split_whitespace().collect();
    let wb: BTreeSet<&str> = b.split_whitespace().collect();
    if wa.is_empty() || wb.is_empty() {
        return 0.0;
    }
    let intersection = wa.intersection(&wb).count();
    let union = wa.union(&wb).count();
    intersection as f64 / union as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_detected() {
        let msgs = vec![
            ("user".into(), "find collision q=4".into()),
            ("assistant".into(), "COLLISION FOUND! q=4 collision discovered.".into()),
        ];
        let p = evaluate(&msgs);
        assert!(p.collision_found);
    }

    #[test]
    fn partial_match_extracted() {
        let msgs = vec![
            ("user".into(), "search".into()),
            ("assistant".into(), "best_match=3/4 outputs match".into()),
        ];
        let p = evaluate(&msgs);
        assert_eq!(p.best_partial_match, 3);
    }

    #[test]
    fn stuck_detected_repetitive() {
        let same = "running birthday search... iteration 100000, no collision";
        let mut msgs = vec![("user".into(), "go".into())];
        for _ in 0..8 {
            msgs.push(("assistant".into(), same.into()));
        }
        let p = evaluate(&msgs);
        assert!(p.looks_stuck);
    }

    #[test]
    fn not_stuck_when_progressing() {
        let mut msgs = vec![("user".into(), "go".into())];
        let varied = [
            "running groebner basis attempt 1",
            "trying different variable ordering",
            "basis computed, extracting roots",
            "found candidate! verifying...",
            "no match, trying with fewer variables",
            "reducing system to last 4 rounds",
            "running F4 algorithm with block order",
            "extracted 3 candidate solutions",
        ];
        for v in varied {
            msgs.push(("assistant".into(), v.into()));
        }
        let p = evaluate(&msgs);
        assert!(!p.looks_stuck);
    }
}
