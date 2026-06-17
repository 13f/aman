// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Memory providers for the aman agent framework.
//!
//! Each provider implements [`kernel::memory::MemoryProvider`].
//! Currently the default (and only) backend is [`YantrikdbProvider`].

#![forbid(unsafe_code)]

use kernel::error::AmanResult;
use kernel::memory::MemoryProvider;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub mod config;
pub mod yantrikdb;

pub use config::{EmbeddingConfig, MemoryConfig};
// Re-export from cognitive_llm for convenience — embedder lives alongside
// the other LLM communication implementations (openai, anthropic, embed).
pub use cognitive_llm::embed::OpenAiEmbedder;
pub use yantrikdb::YantrikdbProvider;

/// Thread-safe registry for [`MemoryProvider`] instances.
///
/// Providers are stored by name. Plugins register their providers here,
/// and the runtime resolves the configured provider by name at startup.
pub struct MemoryProviderRegistry {
    providers: RwLock<HashMap<String, Arc<dyn MemoryProvider>>>,
}

impl MemoryProviderRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            providers: RwLock::new(HashMap::new()),
        }
    }

    /// Register a provider. Returns an error if a provider with the same name
    /// already exists.
    pub fn register(&self, provider: Arc<dyn MemoryProvider>) -> AmanResult<()> {
        let mut providers = self.providers.write().expect("memory provider registry write lock");
        let name = provider.name().to_owned();
        if providers.contains_key(&name) {
            return Err(kernel::Error::AlreadyExists {
                name: format!("memory_provider:{name}"),
            });
        }
        providers.insert(name, provider);
        Ok(())
    }

    /// Unregister a provider by name. Returns true if the provider was removed.
    pub fn unregister(&self, provider_name: &str) -> bool {
        let mut providers = self.providers.write().expect("memory provider registry write lock");
        providers.remove(provider_name).is_some()
    }

    /// Look up a provider by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn MemoryProvider>> {
        let providers = self.providers.read().expect("memory provider registry read lock");
        providers.get(name).cloned()
    }

    /// Return all registered provider names, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let providers = self.providers.read().expect("memory provider registry read lock");
        let mut names: Vec<String> = providers.keys().cloned().collect();
        names.sort();
        names
    }

    /// Return the number of registered providers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.read().expect("memory provider registry read lock").len()
    }

    /// Return true if no providers are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for MemoryProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal mock provider for testing the registry.
    struct MockProvider {
        name: String,
    }

    impl MockProvider {
        fn new(name: &str) -> Self {
            Self { name: name.into() }
        }
    }

    #[async_trait::async_trait]
    impl MemoryProvider for MockProvider {
        fn name(&self) -> &str {
            &self.name
        }
    }

    #[test]
    fn registry_new_is_empty() {
        let reg = MemoryProviderRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.names().is_empty());
    }

    #[test]
    fn registry_register_and_get() {
        let reg = MemoryProviderRegistry::new();
        let p: Arc<dyn MemoryProvider> = Arc::new(MockProvider::new("test"));
        reg.register(p).expect("register");

        assert_eq!(reg.len(), 1);
        assert!(!reg.is_empty());
        assert_eq!(reg.names(), vec!["test"]);

        let retrieved = reg.get("test");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name(), "test");
    }

    #[test]
    fn registry_register_duplicate_fails() {
        let reg = MemoryProviderRegistry::new();
        let p1: Arc<dyn MemoryProvider> = Arc::new(MockProvider::new("dup"));
        let p2: Arc<dyn MemoryProvider> = Arc::new(MockProvider::new("dup"));

        reg.register(p1).expect("first register");
        let err = reg.register(p2).unwrap_err();
        assert!(err.to_string().contains("memory_provider:dup"));
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn registry_unregister() {
        let reg = MemoryProviderRegistry::new();
        let p: Arc<dyn MemoryProvider> = Arc::new(MockProvider::new("alpha"));
        reg.register(p).expect("register");

        assert!(reg.unregister("alpha"));
        assert!(reg.is_empty());

        // Unregister non-existent provider returns false
        assert!(!reg.unregister("nonexistent"));
    }

    #[test]
    fn registry_get_returns_none_for_unknown() {
        let reg = MemoryProviderRegistry::new();
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn registry_names_sorted() {
        let reg = MemoryProviderRegistry::new();
        reg.register(Arc::new(MockProvider::new("z")) as Arc<dyn MemoryProvider>)
            .expect("register z");
        reg.register(Arc::new(MockProvider::new("a")) as Arc<dyn MemoryProvider>)
            .expect("register a");
        reg.register(Arc::new(MockProvider::new("m")) as Arc<dyn MemoryProvider>)
            .expect("register m");

        assert_eq!(reg.names(), vec!["a", "m", "z"]);
    }

    #[test]
    fn registry_default_is_empty() {
        let reg = MemoryProviderRegistry::default();
        assert!(reg.is_empty());
    }
}
