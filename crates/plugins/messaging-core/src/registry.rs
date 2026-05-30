// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! [`ChannelRegistry`] — maps source IDs to [`MessageSender`] instances.

use crate::sender::MessageSender;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Registry of active messaging channel senders, keyed by source ID.
///
/// Each platform source registers its sender during [`EventSource::init`] so
/// that the [`ChatReplyHandler`](super::ChatReplyHandler) can look up the
/// right sender when an agent reply is ready.
pub struct ChannelRegistry {
    senders: RwLock<HashMap<String, Arc<dyn MessageSender>>>,
}

impl ChannelRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            senders: RwLock::new(HashMap::new()),
        }
    }

    /// Register a sender for the given source ID.
    ///
    /// # Panics
    /// Panics if a sender is already registered for `source_id`. Each source
    /// must have a unique ID.
    pub fn register(&self, source_id: String, sender: Arc<dyn MessageSender>) {
        let mut guard = self
            .senders
            .write()
            .expect("ChannelRegistry lock poisoned");
        assert!(
            guard.insert(source_id.clone(), sender).is_none(),
            "ChannelRegistry: duplicate source_id {source_id}"
        );
    }

    /// Look up the sender for a given source ID.
    #[must_use]
    pub fn get(&self, source_id: &str) -> Option<Arc<dyn MessageSender>> {
        self.senders
            .read()
            .expect("ChannelRegistry lock poisoned")
            .get(source_id)
            .cloned()
    }

    /// Remove the sender for a given source ID (called during source shutdown).
    pub fn unregister(&self, source_id: &str) {
        self.senders
            .write()
            .expect("ChannelRegistry lock poisoned")
            .remove(source_id);
    }

    /// Return the number of registered senders.
    #[must_use]
    pub fn len(&self) -> usize {
        self.senders
            .read()
            .expect("ChannelRegistry lock poisoned")
            .len()
    }

    /// Return `true` if no senders are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.senders
            .read()
            .expect("ChannelRegistry lock poisoned")
            .is_empty()
    }
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sender::MessageSender;
    use crate::types::ChatTarget;
    use async_trait::async_trait;
    use kernel::AmanResult;

    struct DummySender;

    #[async_trait]
    impl MessageSender for DummySender {
        async fn send_text(&self, _target: &ChatTarget, _text: &str) -> AmanResult<()> {
            Ok(())
        }
    }

    #[test]
    fn register_and_lookup() {
        let registry = ChannelRegistry::new();
        let sender: Arc<dyn MessageSender> = Arc::new(DummySender);
        registry.register("chat:telegram:bot".to_owned(), Arc::clone(&sender));

        let found = registry.get("chat:telegram:bot");
        assert!(found.is_some());

        let missing = registry.get("chat:slack:bot");
        assert!(missing.is_none());

        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn unregister_removes_sender() {
        let registry = ChannelRegistry::new();
        let sender: Arc<dyn MessageSender> = Arc::new(DummySender);
        registry.register("chat:telegram:bot".to_owned(), sender);

        registry.unregister("chat:telegram:bot");
        assert!(registry.get("chat:telegram:bot").is_none());
        assert!(registry.is_empty());
    }

    #[test]
    #[should_panic(expected = "duplicate source_id")]
    fn duplicate_register_panics() {
        let registry = ChannelRegistry::new();
        let s1: Arc<dyn MessageSender> = Arc::new(DummySender);
        let s2: Arc<dyn MessageSender> = Arc::new(DummySender);
        registry.register("chat:telegram:bot".to_owned(), s1);
        registry.register("chat:telegram:bot".to_owned(), s2);
    }
}
