#![forbid(unsafe_code)]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


//! Shared registry for pending tool authorization requests.
//!
//! The LLM tool wrapper registers a `oneshot::Sender` before emitting the
//! `tool_auth_required` event and awaits the receiver. The gateway HTTP
//! endpoint resolves the sender when the user responds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// Shared registry for pending tool authorization requests.
///
/// This is `Clone`-cheap (Arc clone). Passed by reference from `AgentRuntime`
/// to the LLM plugin and HTTP endpoint.
#[derive(Clone)]
pub struct AuthRegistry {
    /// Number of pending requests (for Debug display).
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    /// Session-scoped allow cache: key = `"{tool_name}:{args_hash}"`.
    session_cache: Arc<Mutex<HashMap<String, bool>>>,
}

impl std::fmt::Debug for AuthRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthRegistry")
            .field("pending_count", &self.pending_count())
            .finish()
    }
}

impl AuthRegistry {
    /// Create a new, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            session_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a pending auth request.
    ///
    /// The returned `Receiver` **must** be awaited. The caller should wrap the
    /// await in `tokio::time::timeout(60s, ...)` to prevent hangs.
    pub fn register(&self, auth_id: String) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("AuthRegistry pending lock")
            .insert(auth_id, tx);
        rx
    }

    /// Resolve a pending auth request. Returns `true` if the request existed.
    pub fn resolve(&self, auth_id: &str, approved: bool) -> bool {
        let mut map = self.pending.lock().expect("AuthRegistry pending lock");
        if let Some(tx) = map.remove(auth_id) {
            let _ = tx.send(approved);
            true
        } else {
            false
        }
    }

    /// Remove a timed-out or expired request without resolving it.
    pub fn remove(&self, auth_id: &str) -> bool {
        self.pending
            .lock()
            .expect("AuthRegistry pending lock")
            .remove(auth_id)
            .is_some()
    }

    /// Number of pending (unresolved) auth requests.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .expect("AuthRegistry pending lock")
            .len()
    }

    /// Check the session-scoped allow cache.
    ///
    /// Returns `Some(true)` if previously approved for this session,
    /// `Some(false)` if previously denied, `None` if no cached decision.
    #[must_use]
    pub fn check_session_cache(&self, tool_name: &str, args_hash: &str) -> Option<bool> {
        let key = format!("{tool_name}:{args_hash}");
        self.session_cache
            .lock()
            .expect("AuthRegistry session cache lock")
            .get(&key)
            .copied()
    }

    /// Set the session-scoped allow cache.
    pub fn set_session_cache(&self, tool_name: &str, args_hash: &str, approved: bool) {
        let key = format!("{tool_name}:{args_hash}");
        self.session_cache
            .lock()
            .expect("AuthRegistry session cache lock")
            .insert(key, approved);
    }
}

impl Default for AuthRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Plugin Approval Registry ───────────────────────────────────────

/// Shared registry for pending plugin capability approval requests.
///
/// When a plugin needs user approval for its requested capabilities,
/// the gateway registers a oneshot sender here before emitting the
/// `plugin_auth_required` event. The HTTP/gRPC/stdio endpoint resolves
/// the sender when the user responds via the native auth dialog.
///
/// Separate from [`AuthRegistry`] because plugin approvals are
/// persistent (saved via [`ApprovalCache`]) and keyed by plugin name
/// rather than a transient auth_id.
#[derive(Clone)]
pub struct PluginApprovalRegistry {
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
}

impl std::fmt::Debug for PluginApprovalRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginApprovalRegistry")
            .field("pending_count", &self.pending_count())
            .finish()
    }
}

impl PluginApprovalRegistry {
    /// Create a new, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a pending plugin approval request.
    ///
    /// Returns a [`oneshot::Receiver<bool>`] that resolves to `true` if the
    /// user approved or `false` if denied. The caller should wrap the await
    /// in `tokio::time::timeout()` to prevent hangs if the user never responds.
    pub fn register(&self, plugin_name: String) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("PluginApprovalRegistry pending lock")
            .insert(plugin_name, tx);
        rx
    }

    /// Resolve a pending plugin approval request.
    ///
    /// Returns `true` if the request existed and was resolved, `false` if
    /// no pending request was found for the given plugin name.
    pub fn resolve(&self, plugin_name: &str, approved: bool) -> bool {
        let mut map = self.pending.lock().expect("PluginApprovalRegistry pending lock");
        if let Some(tx) = map.remove(plugin_name) {
            let _ = tx.send(approved);
            true
        } else {
            false
        }
    }

    /// Remove a pending request without resolving (e.g., on timeout).
    pub fn remove(&self, plugin_name: &str) -> bool {
        self.pending
            .lock()
            .expect("PluginApprovalRegistry pending lock")
            .remove(plugin_name)
            .is_some()
    }

    /// Number of pending (unresolved) approval requests.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .expect("PluginApprovalRegistry pending lock")
            .len()
    }
}

impl Default for PluginApprovalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_register_and_resolve_approved() {
        let registry = AuthRegistry::new();
        let auth_id = "test-1".to_owned();

        let rx = registry.register(auth_id.clone());

        let resolved = registry.resolve(&auth_id, true);
        assert!(resolved, "should find and resolve the request");

        let result = tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .expect("timeout waiting for auth response")
            .expect("oneshot should send");
        assert!(result, "should be approved");
    }

    #[tokio::test]
    async fn test_register_and_resolve_denied() {
        let registry = AuthRegistry::new();
        let auth_id = "test-2".to_owned();

        let rx = registry.register(auth_id.clone());

        let resolved = registry.resolve(&auth_id, false);
        assert!(resolved);

        let result = tokio::time::timeout(Duration::from_secs(1), rx)
            .await
            .expect("timeout")
            .expect("oneshot should send");
        assert!(!result, "should be denied");
    }

    #[test]
    fn test_resolve_nonexistent() {
        let registry = AuthRegistry::new();
        assert!(!registry.resolve("ghost", true));
    }

    #[test]
    fn test_pending_count() {
        let registry = AuthRegistry::new();
        assert_eq!(registry.pending_count(), 0);

        let _rx = registry.register("a".to_owned());
        assert_eq!(registry.pending_count(), 1);

        let _rx = registry.register("b".to_owned());
        assert_eq!(registry.pending_count(), 2);

        registry.resolve("a", true);
        assert_eq!(registry.pending_count(), 1);
    }

    #[test]
    fn test_session_cache() {
        let registry = AuthRegistry::new();

        assert_eq!(registry.check_session_cache("exec", "abc123"), None);

        registry.set_session_cache("exec", "abc123", true);
        assert_eq!(registry.check_session_cache("exec", "abc123"), Some(true));

        registry.set_session_cache("exec", "abc123", false);
        assert_eq!(registry.check_session_cache("exec", "abc123"), Some(false));
    }

    #[test]
    fn test_remove_pending() {
        let registry = AuthRegistry::new();
        let _rx = registry.register("test".to_owned());
        assert!(registry.remove("test"));
        assert!(!registry.remove("test"));
    }
}
