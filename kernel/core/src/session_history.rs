// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use crate::react::ChatMessage;
use std::collections::HashMap;
use std::sync::RwLock;

/// Session history store interface.
///
/// Provides per-session conversation history with configurable capacity limits.
pub trait SessionHistoryStore: Send + Sync {
    /// Append a message to the session's history.
    /// Trims oldest messages if over capacity.
    fn append(&self, session_id: &str, msg: ChatMessage);

    /// Append multiple messages, trimming if over capacity.
    fn extend(&self, session_id: &str, messages: Vec<ChatMessage>);

    /// Get the current history for a session.
    fn get(&self, session_id: &str) -> Vec<ChatMessage>;

    /// Clear history for a session.
    fn clear(&self, session_id: &str);

    /// Set maximum messages per session.
    fn set_max_messages(&self, max: usize);
}

/// In-memory implementation of [`SessionHistoryStore`].
///
/// Stores histories in a `RwLock<HashMap>`, with optional per-session
/// message cap. Default max is 100 messages per session.
pub struct InMemorySessionHistory {
    histories: RwLock<HashMap<String, Vec<ChatMessage>>>,
    max_messages: RwLock<usize>,
}

impl InMemorySessionHistory {
    pub fn new() -> Self {
        Self {
            histories: RwLock::new(HashMap::new()),
            max_messages: RwLock::new(100),
        }
    }

    pub fn with_max_messages(max: usize) -> Self {
        Self {
            histories: RwLock::new(HashMap::new()),
            max_messages: RwLock::new(max),
        }
    }

    fn trim(&self, history: &mut Vec<ChatMessage>) {
        let max = *self.max_messages.read().expect("max_messages lock");
        if history.len() > max {
            *history = history.split_off(history.len() - max);
        }
    }
}

impl Default for InMemorySessionHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionHistoryStore for InMemorySessionHistory {
    fn append(&self, session_id: &str, msg: ChatMessage) {
        let mut histories = self.histories.write().expect("histories lock");
        let history = histories.entry(session_id.to_owned()).or_default();
        history.push(msg);
        self.trim(history);
    }

    fn extend(&self, session_id: &str, messages: Vec<ChatMessage>) {
        let mut histories = self.histories.write().expect("histories lock");
        let history = histories.entry(session_id.to_owned()).or_default();
        history.extend(messages);
        self.trim(history);
    }

    fn get(&self, session_id: &str) -> Vec<ChatMessage> {
        let histories = self.histories.read().expect("histories lock");
        histories.get(session_id).cloned().unwrap_or_default()
    }

    fn clear(&self, session_id: &str) {
        let mut histories = self.histories.write().expect("histories lock");
        histories.remove(session_id);
    }

    fn set_max_messages(&self, max: usize) {
        let mut m = self.max_messages.write().expect("max_messages lock");
        *m = max;
    }
}
