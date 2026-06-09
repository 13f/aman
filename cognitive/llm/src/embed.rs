// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! OpenAI-compatible embedding client (`/v1/embeddings`).
//!
//! Implements [`yantrikdb::types::Embedder`] so it can be plugged directly
//! into YantrikDB's vector store. Works with any OpenAI-compatible server:
//! OpenAI, Ollama (≥v0.1.28), oMLX, LM Studio, etc.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tracing::{debug, warn};

use crate::net_proxy::agent_for;

// ---------------------------------------------------------------------------
// Tuning knobs
// ---------------------------------------------------------------------------

/// Timeout for each individual embed request.
///
/// Set high enough to cover large payloads in eager mode (2924 tokens
/// took ~27s on M4 w/ oMLX). Compiled mode should be faster, but eager
/// fallback is common when `mx.compile` fails.
const EMBED_TIMEOUT_SECS: u64 = 90;

/// Maximum retry attempts for transient errors (connection reset, timeout, 5xx).
const EMBED_MAX_RETRIES: u32 = 2;

/// Backoff base in seconds — grows as `base * (attempt + 1)`.
const RETRY_BACKOFF_BASE_SECS: u64 = 1;

/// Number of consecutive failures before the circuit breaker engages.
const CIRCUIT_BREAKER_THRESHOLD: u64 = 5;

/// Cooldown duration when the circuit breaker is open.
const CIRCUIT_COOLDOWN_SECS: u64 = 30;

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
    /// Count of consecutive failures — reset on success. Guards against
    /// hammering a broken backend (circuit breaker pattern).
    consecutive_failures: AtomicU64,
    /// Serialize embedding requests so only one HTTP call is in flight
    /// at a time. Prevents overwhelming local servers (oMLX, Ollama)
    /// that process requests sequentially.
    embed_lock: Mutex<()>,
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
            consecutive_failures: AtomicU64::new(0),
            embed_lock: Mutex::new(()),
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
        // -- concurrency gate: one embed request at a time -----------------
        // Local embedding servers (oMLX, Ollama) process requests
        // sequentially. Multiple concurrent agents sending large payloads
        // (2924+ tokens) cause all but the first to time out. Serialize so
        // each request gets the full attention of the server.
        let _guard = match self.embed_lock.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                // Mutex poisoned — a previous holder panicked. Recover.
                warn!("Embedder lock poisoned — recovering");
                poisoned.into_inner()
            }
        };

        // -- circuit breaker: if we've had N consecutive failures, pause ----
        let fails = self.consecutive_failures.load(Ordering::Relaxed);
        if fails >= CIRCUIT_BREAKER_THRESHOLD {
            warn!(
                consecutive_failures = fails,
                cooldown_secs = CIRCUIT_COOLDOWN_SECS,
                url = %self.url,
                "Embedder circuit breaker open — backing off",
            );
            std::thread::sleep(Duration::from_secs(CIRCUIT_COOLDOWN_SECS));
        }

        // -- retry loop for transient errors -------------------------------
        let body = serde_json::json!({
            "input": text,
            "model": self.model,
        });

        let mut last_error: Option<Box<dyn std::error::Error + Send + Sync>> = None;

        for attempt in 0..=EMBED_MAX_RETRIES {
            if attempt > 0 {
                let delay = Duration::from_secs(RETRY_BACKOFF_BASE_SECS * attempt as u64);
                debug!(
                    attempt,
                    delay_ms = delay.as_millis(),
                    url = %self.url,
                    "Retrying embed request",
                );
                std::thread::sleep(delay);
            }

            match self.try_embed(&body) {
                Ok(vec) => {
                    // Success — reset circuit breaker.
                    self.consecutive_failures.store(0, Ordering::Relaxed);
                    return Ok(vec);
                }
                Err(e) => {
                    let is_transient = is_transient_error(e.as_ref());
                    if !is_transient {
                        // Non-transient error (e.g. 4xx, bad JSON, wrong
                        // model name). Don't retry — fail fast.
                        self.consecutive_failures
                            .fetch_add(1, Ordering::Relaxed);
                        return Err(e);
                    }
                    if attempt == EMBED_MAX_RETRIES {
                        // Exhausted retries — circuit breaker tick.
                        let new_fails =
                            self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
                        warn!(
                            consecutive_failures = new_fails,
                            url = %self.url,
                            "Embedder exhausted retries; circuit breaker armed",
                        );
                        return Err(e);
                    }
                    last_error = Some(e);
                }
            }
        }

        // Unreachable (loop always returns or breaks), but satisfy the
        // compiler.
        Err(last_error.unwrap())
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

impl OpenAiEmbedder {
    /// Single embed attempt (no retry, no circuit breaker).
    fn try_embed(
        &self,
        body: &serde_json::Value,
    ) -> std::result::Result<Vec<f32>, Box<dyn std::error::Error + Send + Sync>> {
        let mut req = self
            .agent
            .post(&self.url)
            .set("Content-Type", "application/json")
            .timeout(Duration::from_secs(EMBED_TIMEOUT_SECS));

        if !self.api_key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {}", self.api_key));
        }

        let resp: Value = req
            .send_json(body.clone())
            .map_err(|e| {
                warn!(error = %e, url = %self.url, "Embedder HTTP error");
                Box::new(e) as Box<dyn std::error::Error + Send + Sync>
            })?
            .into_json()
            .map_err(|e| {
                let err_str = e.to_string();
                // Distinguish read timeouts (IO error while pulling the
                // response body) from genuine JSON parse failures.
                if err_str.contains("timed out") || err_str.contains("timeout") {
                    warn!(error = %e, url = %self.url, "Embedder read timeout");
                } else {
                    warn!(error = %e, url = %self.url, "Embedder JSON parse error");
                }
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
}

// ---------------------------------------------------------------------------
// Error classification
// ---------------------------------------------------------------------------

/// Decide whether an error from `try_embed` is transient (worth retrying).
///
/// Transient errors: network blips (connection reset, timeout, broken pipe,
/// refused), DNS failures, 5xx server errors, 429 rate-limit.
///
/// Non-transient errors: 4xx client errors (bad model name, bad API key),
/// JSON parse failures, unexpected response shape.
fn is_transient_error(e: &(dyn std::error::Error + Send + Sync)) -> bool {
    let msg = e.to_string().to_lowercase();

    // IO / network signals worth retrying.
    if msg.contains("connection reset")
        || msg.contains("os error 54")
        || msg.contains("connection refused")
        || msg.contains("os error 61")
        || msg.contains("broken pipe")
        || msg.contains("os error 32")
        || msg.contains("timed out")
        || msg.contains("timeout")
        || msg.contains("dns error")
        || msg.contains("network error")
    {
        return true;
    }

    // ureq boxes HTTP status responses — 5xx and 429 are transient.
    if msg.contains("http 5") || msg.contains("http 429") || msg.contains("status 5") || msg.contains("status 429")
    {
        return true;
    }

    // JSON parse errors, missing fields, 4xx responses — not transient.
    false
}
