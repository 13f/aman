// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! [`ChatSessionStore`] — maps session IDs to [`ChatTarget`] for reply routing.

use crate::types::ChatTarget;
use std::collections::HashMap;
use std::sync::RwLock;

/// In-memory store mapping session IDs to chat targets.
///
/// Populated by [`ChatIngestionHandler`](super::ChatIngestionHandler) when a
/// chat message arrives, and read by [`ChatReplyHandler`](super::ChatReplyHandler)
/// when an agent reply is ready to be sent back.
///
/// Session IDs are deterministic (`chat:{platform}:{chat_id}`) and stable
/// across restarts — no persistence is required (the store repopulates on
/// the next message).
pub struct ChatSessionStore {
    sessions: RwLock<HashMap<String, ChatTarget>>,
}

impl ChatSessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// Store a chat target for a session ID, overwriting any existing entry.
    pub fn store(&self, session_id: String, target: ChatTarget) {
        self.sessions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id, target);
    }

    /// Look up the chat target for a session ID.
    #[must_use]
    pub fn get(&self, session_id: &str) -> Option<ChatTarget> {
        self.sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
    }

    /// Remove a session entry (e.g., when a chat is archived).
    pub fn remove(&self, session_id: &str) {
        self.sessions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
    }

    /// Return the number of active sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Return `true` if no sessions are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    /// List all session IDs currently stored.
    #[must_use]
    pub fn list_ids(&self) -> Vec<String> {
        self.sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }
}

impl Default for ChatSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PlatformKind;

    fn make_target() -> ChatTarget {
        ChatTarget {
            platform: PlatformKind::Telegram,
            chat_id: "12345".to_owned(),
            source_id: "chat:telegram:bot".to_owned(),
            thread_id: None,
        }
    }

    #[test]
    fn store_and_get() {
        let store = ChatSessionStore::new();
        let target = make_target();
        store.store("chat:telegram:12345".to_owned(), target.clone());

        let found = store.get("chat:telegram:12345");
        assert!(found.is_some());
        assert_eq!(found.unwrap().chat_id, "12345");

        assert!(store.get("nonexistent").is_none());
    }

    #[test]
    fn overwrite_existing_session() {
        let store = ChatSessionStore::new();
        let t1 = make_target();

        let t2 = ChatTarget {
            platform: PlatformKind::Telegram,
            chat_id: "67890".to_owned(),
            source_id: "chat:telegram:bot".to_owned(),
            thread_id: Some("thread_1".to_owned()),
        };

        store.store("s1".to_owned(), t1);
        store.store("s1".to_owned(), t2.clone());

        let found = store.get("s1").unwrap();
        assert_eq!(found.chat_id, "67890");
        assert_eq!(found.thread_id, Some("thread_1".to_owned()));
    }

    #[test]
    fn remove_cleans_up() {
        let store = ChatSessionStore::new();
        store.store("s1".to_owned(), make_target());
        assert_eq!(store.len(), 1);

        store.remove("s1");
        assert!(store.is_empty());
        assert!(store.get("s1").is_none());
    }

    #[test]
    fn list_ids_returns_all() {
        let store = ChatSessionStore::new();
        store.store("a".to_owned(), make_target());
        store.store("b".to_owned(), make_target());

        let mut ids = store.list_ids();
        ids.sort();
        assert_eq!(ids, vec!["a", "b"]);
    }
}
