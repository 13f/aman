// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! OpenAI-compatible embedding client (`/v1/embeddings`).
//!
//! Implements [`yantrikdb::types::Embedder`] so it can be plugged directly
//! into YantrikDB's vector store. Works with any OpenAI-compatible server:
//! OpenAI, Ollama (≥v0.1.28), oMLX, LM Studio, etc.

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::net_proxy::agent_for;

/// An embedder that calls a remote OpenAI-compatible `/v1/embeddings` endpoint.
///
/// # Examples
///
/// ```ignore
/// let embedder = OpenAiEmbedder::new(
///     "http://127.0.0.1:11434/v1",  // Ollama
///     "",                            // no API key for local
///     "qwen3-embedding:8b",
///     4096,
/// );
/// ```
pub struct OpenAiEmbedder {
    agent: ureq::Agent,
    url: String,
    api_key: String,
    model: String,
    dim: usize,
    fingerprint: String,
}

impl OpenAiEmbedder {
    /// Create a new OpenAI-compatible embedder.
    ///
    /// `base_url` should NOT include a trailing `/embeddings` segment
    /// (e.g. `"http://localhost:11434/v1"`).
    pub fn new(base_url: &str, api_key: &str, model: &str, dim: usize) -> Self {
        let base = base_url.trim_end_matches('/');
        let url = format!("{base}/embeddings");

        let mut hasher = Sha256::new();
        hasher.update(format!("openai-embed:{base}:{model}").as_bytes());
        let fingerprint = format!("{:x}", hasher.finalize());

        debug!(%url, %model, dim, "Created OpenAiEmbedder");

        Self {
            agent: agent_for(base_url),
            url,
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            dim,
            fingerprint,
        }
    }

    /// Detect the embedding dimension by making a single test API call.
    ///
    /// Returns the dimension on success, or an error if the API is unreachable
    /// or returns an unexpected shape.
    pub fn detect_dim(
        base_url: &str,
        api_key: &str,
        model: &str,
    ) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let base = base_url.trim_end_matches('/');
        let url = format!("{base}/embeddings");

        let body = serde_json::json!({
            "input": "dimension detection probe",
            "model": model,
        });

        let agent = agent_for(base_url);
        let mut req = agent
            .post(&url)
            .set("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(3));

        if !api_key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {api_key}"));
        }

        let resp: Value = req.send_json(body).map_err(|e| {
            format!("embedding API probe failed for {base}/embeddings: {e}")
        })?
        .into_json()
        .map_err(|e| {
            format!("embedding API probe: failed to parse JSON response: {e}")
        })?;

        let dim = resp["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| {
                let body_preview = serde_json::to_string_pretty(&resp)
                    .unwrap_or_else(|_| String::from("(unable to serialize response)"));
                format!(
                    "unexpected embedding API response: missing data[0].embedding array\n\
                     endpoint: {base}/embeddings\n\
                     response body:\n{body_preview}"
                )
            })?
            .len();

        debug!(%url, %model, dim, "Detected embedding dimension");
        Ok(dim)
    }
}

impl yantrikdb::types::Embedder for OpenAiEmbedder {
    fn embed(
        &self,
        text: &str,
    ) -> std::result::Result<Vec<f32>, Box<dyn std::error::Error + Send + Sync>> {
        let body = serde_json::json!({
            "input": text,
            "model": self.model,
        });

        let mut req = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json");

        if !self.api_key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {}", self.api_key));
        }

        let resp: Value = req
            .send_json(body)
            .map_err(|e| {
                warn!(error = %e, url = %self.url, "Embedder HTTP error");
                Box::new(e) as Box<dyn std::error::Error + Send + Sync>
            })?
            .into_json()
            .map_err(|e| {
                warn!(error = %e, url = %self.url, "Embedder JSON parse error");
                Box::new(e) as Box<dyn std::error::Error + Send + Sync>
            })?;

        let vec: Vec<f32> = resp["data"][0]["embedding"]
            .as_array()
            .ok_or_else(|| {
                let msg = format!(
                    "unexpected embedding API response structure: {}",
                    serde_json::to_string_pretty(&resp).unwrap_or_default()
                );
                Box::<dyn std::error::Error + Send + Sync>::from(msg)
            })?
            .iter()
            .map(|v: &serde_json::Value| v.as_f64().unwrap_or(0.0) as f32)
            .collect();

        Ok(vec)
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn fingerprint(&self) -> Option<String> {
        Some(self.fingerprint.clone())
    }

    fn name(&self) -> Option<String> {
        Some(format!("openai-embed:{}", self.model))
    }
}
