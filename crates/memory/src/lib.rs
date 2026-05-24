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
pub mod remote_embedder;
pub mod ollama_embedder;
pub mod yantrikdb;

pub use config::{EmbeddingConfig, MemoryConfig};
pub use remote_embedder::RemoteEmbedder;
pub use ollama_embedder::OllamaEmbedder;
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
