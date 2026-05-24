// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use reqwest::blocking::Client;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

/// A [`yantrikdb::types::Embedder`] that calls a remote OpenAI-compatible
/// `/v1/embeddings` endpoint.
///
/// Useful when:
/// - The bundled potion models are too large to download
/// - You want to use a specific cloud embedding model (e.g. qwen3-embedding-8b)
/// - You already have LM Studio / Ollama / OpenAI serving embeddings locally
pub struct RemoteEmbedder {
    client: Client,
    url: String,
    api_key: String,
    model: String,
    dim: usize,
    fingerprint: String,
}

impl RemoteEmbedder {
    /// Create a new remote embedder.
    ///
    /// `base_url` should NOT include a trailing `/embeddings` segment
    /// (e.g. `"http://localhost:1234/v1"`).
    pub fn new(base_url: &str, api_key: &str, model: &str, dim: usize) -> Self {
        let base = base_url.trim_end_matches('/');
        let url = format!("{base}/embeddings");

        let mut hasher = Sha256::new();
        hasher.update(format!("remote:{base}:{model}").as_bytes());
        let fingerprint = format!("{:x}", hasher.finalize());

        debug!(%url, %model, dim, "Created RemoteEmbedder");

        Self {
            client: Client::new(),
            url,
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            dim,
            fingerprint,
        }
    }

    /// Detect the embedding dimension by making a single test API call.
    ///
    /// Returns the dimension on success, or an error if the API is unreachable.
    pub fn detect_dim(
        base_url: &str,
        api_key: &str,
        model: &str,
    ) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let base = base_url.trim_end_matches('/');
        let url = format!("{base}/embeddings");

        let client = Client::new();
        let body = serde_json::json!({
            "input": "dimension detection probe",
            "model": model,
        });

        let mut req = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body);

        if !api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {api_key}"));
        }

        let resp: Value = req.send()?.json()?;
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

impl yantrikdb::types::Embedder for RemoteEmbedder {
    fn embed(
        &self,
        text: &str,
    ) -> std::result::Result<Vec<f32>, Box<dyn std::error::Error + Send + Sync>> {
        let body = serde_json::json!({
            "input": text,
            "model": self.model,
        });

        let mut req = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .json(&body);

        if !self.api_key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        }

        let resp: Value = req
            .send()
            .and_then(|r| r.json())
            .map_err(|e| {
                warn!(error = %e, url = %self.url, "Remote embedder HTTP error");
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
        Some(format!("remote:{}", self.model))
    }
}
