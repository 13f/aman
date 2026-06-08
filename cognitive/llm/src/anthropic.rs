// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Anthropic LLM provider — implements [`LlmProvider`] for Anthropic's
//! `/v1/messages` API.
//!
//! TODO: Implement the full Anthropic Messages API:
//! - POST https://api.anthropic.com/v1/messages
//! - x-api-key header, anthropic-version header
//! - Streaming via SSE
//! - Tool use blocks
//! - Extended thinking (reasoning_effort)

use std::sync::Arc;

use async_trait::async_trait;

use crate::provider::{LlmChatRequest, LlmProvider, LlmResponse, StreamEvent};

/// Anthropic LLM provider.
///
/// Communicates with Anthropic's `/v1/messages` endpoint.
/// Currently a stub — falls back to an error message until implemented.
pub struct LlmAnthropicProvider {
    #[allow(dead_code)]
    base_url: String,
    #[allow(dead_code)]
    api_key: String,
    #[allow(dead_code)]
    model: String,
}

impl LlmAnthropicProvider {
    /// Create a new Anthropic provider.
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            model: model.to_owned(),
        }
    }
}

#[async_trait]
impl LlmProvider for LlmAnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn chat_completion(
        &self,
        _req: LlmChatRequest,
        _cb: Option<Arc<dyn Fn(StreamEvent) + Send + Sync>>,
    ) -> Result<LlmResponse, String> {
        Err("Anthropic provider is not yet implemented. Use the OpenAI provider for now.".into())
    }
}
