// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

/// Build a ureq Agent, skipping the proxy only for local/LAN hosts.
///
/// System proxies can intercept local traffic even when `no_proxy` is set
/// inconsistently across shells. We only bypass for hosts where it's safe:
/// localhost and private-range IPs (192.168.x.x, 10.x.x.x, 172.16-31.x.x).
fn agent_for(base_url: &str) -> ureq::Agent {
    let host = url_host(base_url);
    if is_local_or_private(&host) {
        ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .build()
    } else {
        ureq::Agent::new()
    }
}

/// Extract the host portion from a URL string.
fn url_host(url: &str) -> String {
    url.trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap_or("localhost")
        .split(':')
        .next()
        .unwrap_or("localhost")
        .to_owned()
}

/// True for localhost and RFC 1918 private addresses.
fn is_local_or_private(host: &str) -> bool {
    if host == "localhost" || host == "127.0.0.1" || host == "[::1]" || host == "::1" {
        return true;
    }
    if let Some(tail) = host.strip_prefix("192.168.") {
        return tail.split('.').all(|s| s.parse::<u8>().is_ok());
    }
    if let Some(tail) = host.strip_prefix("10.") {
        return tail.split('.').all(|s| s.parse::<u8>().is_ok());
    }
    if host.starts_with("172.")
        && let Some(second) = host.split('.').nth(1)
        && let Ok(n) = second.parse::<u8>()
    {
        return (16..=31).contains(&n);
    }
    false
}

/// An embedder that calls Ollama's native `/api/embed` endpoint.
///
/// Ollama uses a different API than OpenAI:
/// - Endpoint: `POST /api/embed`
/// - Request: `{"model": "...", "input": "..."}`
/// - Response: `{"model": "...", "embeddings": [[...], ...]}`
///
/// Note the plural `embeddings` and the embedding array directly on each
/// element (no `data[].embedding` nesting).
pub struct OllamaEmbedder {
    agent: ureq::Agent,
    url: String,
    model: String,
    dim: usize,
    fingerprint: String,
}

/// Strip OpenAI-style path suffixes (`/v1`, `/v1/`) from a base URL so we
/// always hit the Ollama root. Users often add `/v1` for chat compatibility.
fn ollama_root(base_url: &str) -> &str {
    let base = base_url.trim_end_matches('/');
    base.strip_suffix("/v1").unwrap_or(base)
}

impl OllamaEmbedder {
    /// Create a new Ollama embedder.
    ///
    /// `base_url` should be the Ollama server root (e.g. `"http://localhost:11434"`).
    /// Any `/v1` suffix is automatically stripped for the Ollama native API.
    pub fn new(base_url: &str, model: &str, dim: usize) -> Self {
        let base = ollama_root(base_url);
        let url = format!("{base}/api/embed");

        let mut hasher = Sha256::new();
        hasher.update(format!("ollama:{base}:{model}").as_bytes());
        let fingerprint = format!("{:x}", hasher.finalize());

        debug!(%url, %model, dim, "Created OllamaEmbedder");

        Self {
            agent: agent_for(base_url),
            url,
            model: model.to_owned(),
            dim,
            fingerprint,
        }
    }

    /// Detect the embedding dimension by making a single probe call to `/api/embed`.
    ///
    /// Returns the dimension on success, or an error if the API is unreachable
    /// or returns an unexpected shape.
    pub fn detect_dim(
        base_url: &str,
        model: &str,
    ) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let base = ollama_root(base_url);
        let url = format!("{base}/api/embed");

        let agent = agent_for(base_url);
        let body = serde_json::json!({
            "model": model,
            "input": "dimension detection probe",
        });

        let resp: Value = match agent
            .post(&url)
            .set("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(3))
            .send_json(body)
        {
            Ok(r) => {
                r.into_json().map_err(|e| {
                    format!("Ollama /api/embed returned non-JSON response: {e}")
                })?
            }
            Err(ureq::Error::Status(status, resp)) => {
                let text = resp.into_string().unwrap_or_default();
                let preview = if text.len() > 500 { &text[..500] } else { &text };
                return Err(format!(
                    "Ollama /api/embed returned HTTP {status}, not JSON:\n{preview}"
                ).into());
            }
            Err(e) => {
                return Err(format!(
                    "Ollama /api/embed request failed: {e}\nCheck that Ollama is running at {url}"
                ).into());
            }
        };

        let dim = resp["embeddings"][0]
            .as_array()
            .ok_or_else(|| {
                let body_preview = serde_json::to_string_pretty(&resp)
                    .unwrap_or_else(|_| String::from("(unable to serialize response)"));
                format!(
                    "unexpected Ollama embedding response: missing embeddings[0] array\n\
                     endpoint: {url}\n\
                     response body:\n{body_preview}"
                )
            })?
            .len();

        debug!(%url, %model, dim, "Detected Ollama embedding dimension");
        Ok(dim)
    }
}

impl yantrikdb::types::Embedder for OllamaEmbedder {
    fn embed(
        &self,
        text: &str,
    ) -> std::result::Result<Vec<f32>, Box<dyn std::error::Error + Send + Sync>> {
        let body = serde_json::json!({
            "model": self.model,
            "input": text,
        });

        let resp: Value = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| {
                warn!(error = %e, url = %self.url, "Ollama embedder HTTP error");
                Box::new(e) as Box<dyn std::error::Error + Send + Sync>
            })?
            .into_json()
            .map_err(|e| {
                warn!(error = %e, url = %self.url, "Ollama embedder JSON parse error");
                Box::new(e) as Box<dyn std::error::Error + Send + Sync>
            })?;

        let vec: Vec<f32> = resp["embeddings"][0]
            .as_array()
            .ok_or_else(|| {
                let msg = format!(
                    "unexpected Ollama embedding response structure: {}",
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
        Some(format!("ollama:{}", self.model))
    }
}
