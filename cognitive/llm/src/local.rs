// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Local LLM provider — implements [`LlmProvider`] for locally-hosted
//! OpenAI-compatible endpoints (Ollama, llama.cpp server, vLLM, etc.).
//!
//! Uses the same OpenAI-compatible `/v1/chat/completions` API but with
//! local-friendly defaults: no API key required, configurable base URL,
//! and support for quirks common in local model servers.

use std::sync::Arc;

use async_trait::async_trait;
use crate::openai::LlmOpenaiProvider;
use crate::provider::{LlmChatRequest, LlmProvider, LlmResponse, StreamEvent};

/// Default base URL for locally-hosted OpenAI-compatible endpoints.
/// Ollama exposes this at `http://localhost:11434/v1`.
const DEFAULT_LOCAL_BASE_URL: &str = "http://localhost:11434/v1";

/// A provider for locally-hosted LLM endpoints.
///
/// Wraps [`LlmOpenaiProvider`] with local-friendly defaults. Supports both
/// streaming and non-streaming chat completions via the OpenAI-compatible
/// API format. The model is specified per-request in [`LlmChatRequest::model`].
///
/// # Example
///
/// ```ignore
/// let provider = LlmLocalProvider::new("http://localhost:8080/v1");
/// let req = LlmChatRequest { model: "llama3".into(), ... };
/// let response = provider.chat_completion(req, None).await?;
/// ```
pub struct LlmLocalProvider {
    inner: LlmOpenaiProvider,
}

impl LlmLocalProvider {
    /// Create a new local provider.
    ///
    /// `base_url` is the root API URL (e.g. `http://localhost:11434/v1` for
    /// Ollama, `http://localhost:8080/v1` for llama.cpp server). If empty,
    /// defaults to the standard Ollama endpoint.
    pub fn new(base_url: &str) -> Self {
        let url = if base_url.is_empty() {
            DEFAULT_LOCAL_BASE_URL.to_owned()
        } else {
            base_url.trim_end_matches('/').to_owned()
        };
        // Local endpoints typically don't require an API key.
        Self {
            inner: LlmOpenaiProvider::new("local".to_owned(), url),
        }
    }

    /// Create a local provider with a custom API key.
    ///
    /// Some setups (e.g., vLLM with `--api-key`) require authentication.
    pub fn with_api_key(base_url: &str, api_key: &str) -> Self {
        let url = if base_url.is_empty() {
            DEFAULT_LOCAL_BASE_URL.to_owned()
        } else {
            base_url.trim_end_matches('/').to_owned()
        };
        Self {
            inner: LlmOpenaiProvider::new(api_key.to_owned(), url),
        }
    }
}

#[async_trait]
impl LlmProvider for LlmLocalProvider {
    fn name(&self) -> &str {
        "local"
    }

    fn base_url(&self) -> &str {
        self.inner.base_url()
    }

    async fn chat_completion(
        &self,
        req: LlmChatRequest,
        cb: Option<Arc<dyn Fn(StreamEvent) + Send + Sync>>,
    ) -> Result<LlmResponse, String> {
        // The OpenAI provider handles the OpenAI-compatible API directly.
        // Local endpoints speak the same protocol, so we just delegate.
        self.inner.chat_completion(req, cb).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name_is_local() {
        let provider = LlmLocalProvider::new("http://localhost:1234/v1");
        assert_eq!(provider.name(), "local");
    }

    #[test]
    fn empty_base_url_defaults_to_ollama() {
        let provider = LlmLocalProvider::new("");
        assert_eq!(provider.name(), "local");
    }

    #[test]
    fn trailing_slash_stripped() {
        let provider = LlmLocalProvider::new("http://localhost:8080/v1/");
        assert_eq!(provider.name(), "local");
    }

    #[test]
    fn custom_api_key() {
        let provider =
            LlmLocalProvider::with_api_key("http://localhost:8000/v1", "secret");
        assert_eq!(provider.name(), "local");
    }
}
