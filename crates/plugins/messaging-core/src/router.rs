// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! [`StickyAgentRouter`] — resolves which agent a chat message should be
//! routed to based on @mention affinity.
//!
//! # Behaviour
//!
//! 1. If the message text contains ``@agentname`` matching a known agent,
//!    the affinity for that `(platform, chat_id)` is updated and the message
//!    is routed to that agent.
//! 2. Otherwise, the last-affined agent for that chat is used.
//! 3. If no affinity exists, the configured default agent is used.
//!
//! Affinity has **no TTL** — it persists until an explicit @mention switches
//! it or the process restarts. Session continuity (context) is handled by
//! the agent's own session store via the stable `session_id`.

use crate::types::PlatformKind;
use std::collections::HashMap;
use std::sync::RwLock;

/// Result of resolving a chat message to a target agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterResolution {
    /// The resolved agent ID.
    pub agent_id: String,
    /// `true` if an explicit @mention was detected in this message.
    pub was_mentioned: bool,
}

/// Sticky agent router — maintains `(platform, chat_id) → agent_id` affinity.
pub struct StickyAgentRouter {
    /// `(platform, chat_id)` → last-mentioned agent ID.
    affinity: RwLock<HashMap<(PlatformKind, String), String>>,
    /// Ordered list of known agent IDs for @mention matching.
    known_agents: Vec<String>,
    /// Default agent when no affinity exists and no @mention is detected.
    default_agent: String,
}

impl StickyAgentRouter {
    /// Create a new router.
    ///
    /// `known_agents` should be the list of agent IDs that users can @mention.
    /// `default_agent` is used when no affinity has been established.
    #[must_use]
    pub fn new(known_agents: Vec<String>, default_agent: String) -> Self {
        Self {
            affinity: RwLock::new(HashMap::new()),
            known_agents,
            default_agent,
        }
    }

    /// Resolve the target agent for an incoming chat message.
    ///
    /// Returns the agent ID and whether an @mention was detected.
    pub fn resolve(&self, platform: PlatformKind, chat_id: &str, text: &str) -> RouterResolution {
        // 1. Scan for @mentions of known agents.
        for agent in &self.known_agents {
            if contains_mention(text, agent) {
                // Update affinity for this chat.
                let key = (platform, chat_id.to_owned());
                self.affinity
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key, agent.clone());
                return RouterResolution {
                    agent_id: agent.clone(),
                    was_mentioned: true,
                };
            }
        }

        // 2. Fall back to existing affinity.
        {
            let guard = self
                .affinity
                .read()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(agent_id) = guard.get(&(platform, chat_id.to_owned())) {
                return RouterResolution {
                    agent_id: agent_id.clone(),
                    was_mentioned: false,
                };
            }
        }

        // 3. Fall back to default.
        RouterResolution {
            agent_id: self.default_agent.clone(),
            was_mentioned: false,
        }
    }

    /// Manually set the affinity for a chat (used by HTTP API or admin commands).
    pub fn set_affinity(&self, platform: PlatformKind, chat_id: &str, agent_id: &str) {
        self.affinity
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert((platform, chat_id.to_owned()), agent_id.to_owned());
    }

    /// Clear the affinity for a chat (reverts to default agent on next message).
    pub fn clear_affinity(&self, platform: PlatformKind, chat_id: &str) {
        self.affinity
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(platform, chat_id.to_owned()));
    }

    /// Return the current affinity for a chat, if any.
    #[must_use]
    pub fn get_affinity(&self, platform: PlatformKind, chat_id: &str) -> Option<String> {
        self.affinity
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(platform, chat_id.to_owned()))
            .cloned()
    }

    /// Return the number of active affinities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.affinity
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Return `true` if no affinities are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.affinity
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
}

/// Check whether `text` contains an @mention of `agent_name`.
///
/// Matches patterns like:
/// - `@agentname` (plain text, Telegram, Matrix)
/// - `@agentname ` (trailing space)
/// - `@agentname\n` (end of line)
/// - `@agentname，` (Chinese comma)
/// - message starting with `@agentname`
fn contains_mention(text: &str, agent_name: &str) -> bool {
    let pattern = format!("@{}", agent_name);
    if let Some(pos) = text.find(&pattern) {
        // The '@' must be at start-of-text or preceded by whitespace/punctuation.
        if pos > 0 {
            let preceding = text[..pos].chars().next_back().unwrap_or('\0');
            if !is_word_boundary(preceding) {
                return false;
            }
        }
        // The agent name must be at end-of-text or followed by whitespace/punctuation.
        let after = pos + pattern.len();
        if after >= text.len() {
            return true;
        }
        let following = text[after..].chars().next().unwrap_or('\0');
        is_word_boundary(following)
    } else {
        false
    }
}

fn is_word_boundary(ch: char) -> bool {
    matches!(
        ch,
        ' ' | '\n' | '\t' | '\r'
            | ',' | '.' | '!' | '?' | ':' | ';'
            | '，' | '。' | '：' | '、' | '！' | '？' | '；'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> StickyAgentRouter {
        StickyAgentRouter::new(
            vec!["alice".to_owned(), "bob".to_owned(), "charlie".to_owned()],
            "alice".to_owned(),
        )
    }

    // ── Mention detection ──────────────────────────────────────────

    #[test]
    fn detects_plain_mention() {
        assert!(contains_mention("@alice help me", "alice"));
    }

    #[test]
    fn detects_mention_at_start() {
        assert!(contains_mention("@alice", "alice"));
    }

    #[test]
    fn detects_mention_followed_by_punctuation() {
        assert!(contains_mention("@alice, help", "alice"));
        assert!(contains_mention("@alice. do it", "alice"));
        assert!(contains_mention("hey @alice! go", "alice"));
        assert!(contains_mention("@alice：帮我", "alice"));
        assert!(contains_mention("@alice，你好", "alice"));
    }

    #[test]
    fn rejects_partial_mention() {
        // "@alice2" should NOT match agent "alice"
        assert!(!contains_mention("@alice2 is here", "alice"));
    }

    #[test]
    fn rejects_midword_mention() {
        // "foo@alice" is not an @mention
        assert!(!contains_mention("foo@alice bar", "alice"));
    }

    // ── Affinity routing ────────────────────────────────────────────

    #[test]
    fn no_mention_no_affinity_returns_default() {
        let r = router();
        let res = r.resolve(PlatformKind::Telegram, "chat_1", "hello");
        assert_eq!(res.agent_id, "alice");
        assert!(!res.was_mentioned);
    }

    #[test]
    fn mention_switches_affinity() {
        let r = router();
        let res = r.resolve(PlatformKind::Telegram, "chat_1", "@bob help me");
        assert_eq!(res.agent_id, "bob");
        assert!(res.was_mentioned);
    }

    #[test]
    fn subsequent_message_uses_affinity() {
        let r = router();

        // First: @mention bob
        let res1 = r.resolve(PlatformKind::Telegram, "chat_1", "@bob what's up");
        assert_eq!(res1.agent_id, "bob");
        assert!(res1.was_mentioned);

        // Second: no mention → still bob
        let res2 = r.resolve(PlatformKind::Telegram, "chat_1", "tell me more");
        assert_eq!(res2.agent_id, "bob");
        assert!(!res2.was_mentioned);
    }

    #[test]
    fn mention_switches_affinity_mid_conversation() {
        let r = router();

        r.resolve(PlatformKind::Telegram, "chat_1", "@bob help");
        r.resolve(PlatformKind::Telegram, "chat_1", "more details");

        // Switch to charlie
        let res = r.resolve(PlatformKind::Telegram, "chat_1", "@charlie take over");
        assert_eq!(res.agent_id, "charlie");
        assert!(res.was_mentioned);

        // Subsequent messages go to charlie
        let res = r.resolve(PlatformKind::Telegram, "chat_1", "continue pls");
        assert_eq!(res.agent_id, "charlie");
        assert!(!res.was_mentioned);
    }

    #[test]
    fn different_chats_have_independent_affinity() {
        let r = router();

        r.resolve(PlatformKind::Telegram, "chat_1", "@bob hello");
        r.resolve(PlatformKind::Telegram, "chat_2", "@charlie hi");

        assert_eq!(
            r.get_affinity(PlatformKind::Telegram, "chat_1"),
            Some("bob".to_owned())
        );
        assert_eq!(
            r.get_affinity(PlatformKind::Telegram, "chat_2"),
            Some("charlie".to_owned())
        );
    }

    #[test]
    fn set_and_clear_affinity() {
        let r = router();

        r.set_affinity(PlatformKind::Slack, "C123", "bob");
        assert_eq!(
            r.get_affinity(PlatformKind::Slack, "C123"),
            Some("bob".to_owned())
        );

        r.clear_affinity(PlatformKind::Slack, "C123");
        assert_eq!(r.get_affinity(PlatformKind::Slack, "C123"), None);
    }

    #[test]
    fn len_tracks_entry_count() {
        let r = router();
        assert!(r.is_empty());

        r.set_affinity(PlatformKind::Telegram, "a", "alice");
        r.set_affinity(PlatformKind::Telegram, "b", "bob");
        assert_eq!(r.len(), 2);

        r.clear_affinity(PlatformKind::Telegram, "a");
        assert_eq!(r.len(), 1);
    }
}
