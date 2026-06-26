// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Auto-read LLM responses via TTS (text-to-speech).
//!
//! When `desktop.auto_read` is enabled and `llm.tts` is configured,
//! this module intercepts `agent:reply_ready` SSE events, extracts the
//! assistant's final reply, generates a summary (LLM or heuristic),
//! synthesizes speech via the local TTS API, and plays the audio.

use secret::SecretBackend;
use serde_json::Value;
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;

/// LLM summary timeout — fall back to heuristic if it takes longer.
const SUMMARY_TIMEOUT_SECS: u64 = 5;

/// Handles automatic TTS reading of LLM responses.
pub struct AutoReader {
    /// Base URL of the resolved TTS provider.
    tts_base_url: String,
    /// Provider-specific TTS model name for API calls.
    api_model_id: String,
    /// Whether auto-read is enabled.
    enabled: bool,
    /// Cancellation flag — set to `true` when new speech should preempt old.
    cancel_flag: Arc<AtomicBool>,
    /// Guard that ensures only one playback at a time.
    playback_lock: Arc<Mutex<()>>,
    /// Resolved summary LLM info (None = use heuristic only).
    summary: Option<SummaryLlm>,
}

/// Resolved summary LLM endpoint + credentials.
struct SummaryLlm {
    base_url: String,
    api_model_id: String,
    api_key: Option<String>,
}

impl AutoReader {
    /// Create an `AutoReader` from the aman config.
    ///
    /// Returns `None` when:
    /// - `llm.tts` is not configured
    /// - `desktop.auto_read` is not enabled
    /// - No provider lists the TTS model
    pub fn from_config() -> Option<Self> {
        let cfg = config::AmanConfig::from_default_path().ok()?;
        let tts_model_id = cfg.llm.as_ref()?.tts.as_ref()?;
        let auto_read = cfg.desktop.as_ref().map(|d| d.auto_read).unwrap_or(false);
        if !auto_read {
            return None;
        }

        let (base_url, api_model_id) = resolve_tts_provider(&cfg, tts_model_id)?;

        // Resolve summary LLM if configured.
        let summary = cfg.llm.as_ref().and_then(|l| l.summary.as_ref()).and_then(|sid| {
            resolve_summary_llm(&cfg, sid)
        });

        tracing::info!(
            tts_model = %tts_model_id,
            tts_api_model = %api_model_id,
            tts_base_url = %base_url,
            has_summary_llm = summary.is_some(),
            "tts_auto_reader: enabled"
        );
        Some(Self {
            tts_base_url: base_url,
            api_model_id,
            enabled: true,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            playback_lock: Arc::new(Mutex::new(())),
            summary,
        })
    }

    /// Handle an `agent:reply_ready` event payload.
    pub async fn on_reply_ready(self: &Arc<Self>, payload: Value) {
        if !self.enabled {
            return;
        }

        let reply = match payload.get("reply").and_then(|v| v.as_str()) {
            Some(r) => r,
            None => return,
        };
        if reply.trim().is_empty() {
            return;
        }

        if payload.get("background").and_then(|v| v.as_bool()).unwrap_or(false) {
            return;
        }

        // Generate summary: LLM if available, fall back to heuristic.
        let summary = self.summarize_reply(reply).await;
        tracing::info!(
            reply_len = reply.len(),
            summary_len = summary.len(),
            "tts_auto_reader: synthesizing speech"
        );

        let audio = match self.synthesize(&summary).await {
            Ok(a) => a,
            Err(e) => {
                tracing::error!(error = %e, "tts_auto_reader: synthesis failed");
                return;
            }
        };

        if let Err(e) = self.play_audio(audio).await {
            tracing::error!(error = %e, "tts_auto_reader: playback failed");
        }
    }

    /// Summarize reply text — LLM with timeout, falling back to heuristic.
    async fn summarize_reply(&self, reply: &str) -> String {
        if let Some(ref sl) = self.summary {
            match tokio::time::timeout(
                std::time::Duration::from_secs(SUMMARY_TIMEOUT_SECS),
                llm_summarize(sl, reply),
            )
            .await
            {
                Ok(Ok(s)) if !s.trim().is_empty() => {
                    tracing::info!(source = "llm", len = s.len(), "tts_auto_reader: summary");
                    return s;
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "tts_auto_reader: LLM summary failed, fallback");
                }
                Err(_) => {
                    tracing::warn!("tts_auto_reader: LLM summary timed out, fallback");
                }
                _ => {
                    tracing::warn!("tts_auto_reader: LLM summary empty, fallback");
                }
            }
        }
        // No summary LLM configured or it failed — read the original text.
        tracing::info!(source = "passthrough", len = reply.len(), "tts_auto_reader: no summary, reading full text");
        reply.to_owned()
    }

    /// Call the TTS API to synthesize speech from text.
    ///
    /// Uses the OpenAI-compatible `/v1/audio/speech` endpoint.
    /// Returns the raw audio bytes (MP3 or WAV depending on the model).
    async fn synthesize(&self, text: &str) -> Result<Vec<u8>, String> {
        let url = format!(
            "{}/audio/speech",
            self.tts_base_url.trim_end_matches('/')
        );

        let body = serde_json::json!({
            "model": self.api_model_id,
            "input": text,
            "response_format": "mp3",
        });

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .no_proxy()
            .build()
            .map_err(|e| format!("tts http client: {e}"))?;

        let resp = client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("tts api request: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            return Err(format!("tts api error ({status}): {body_text}"));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("tts read response: {e}"))?
            .to_vec();

        if bytes.is_empty() {
            return Err("tts returned empty audio".to_owned());
        }

        Ok(bytes)
    }

    /// Play audio bytes through the default audio output.
    ///
    /// Cancels any currently-playing audio (new speech preempts old),
    /// then plays the new audio. Blocks until playback finishes or is cancelled.
    async fn play_audio(&self, audio_bytes: Vec<u8>) -> Result<(), String> {
        // Signal cancellation for any currently-running playback.
        self.cancel_flag.store(true, Ordering::SeqCst);

        // Acquire the playback lock — ensures only one playback at a time.
        // We hold this lock for the duration of playback; new callers wait
        // here until we're done or cancelled.
        let _guard = self.playback_lock.lock().await;

        // Reset cancellation flag (we're starting fresh).
        self.cancel_flag.store(false, Ordering::SeqCst);

        let cancel_flag = self.cancel_flag.clone();

        // Run audio decoding + playback on a blocking thread.
        tokio::task::spawn_blocking(move || {
            let cursor = Cursor::new(audio_bytes);
            let source = match rodio::Decoder::new(cursor) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "tts_auto_reader: decode audio failed");
                    return;
                }
            };

            let (_stream, stream_handle) = match rodio::OutputStream::try_default() {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "tts_auto_reader: open audio stream failed");
                    return;
                }
            };
            let sink = match rodio::Sink::try_new(&stream_handle) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(error = %e, "tts_auto_reader: create sink failed");
                    return;
                }
            };
            sink.append(source);

            // Poll until playback finishes or cancellation is requested.
            while !sink.empty() {
                if cancel_flag.load(Ordering::SeqCst) {
                    sink.stop();
                    tracing::info!("tts_auto_reader: playback cancelled (new speech)");
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        })
        .await
        .map_err(|e| format!("tts playback join error: {e}"))
    }
}

/// Resolve the TTS provider for a given global model ID.
///
/// Scans all providers in the config for one whose `models` list contains
/// the given model ID. Returns the provider's `base_url` and the
/// provider-specific `model_id` to use in API calls.
fn resolve_tts_provider(cfg: &config::AmanConfig, model_id: &str) -> Option<(String, String)> {
    for provider in cfg.providers.values() {
        for entry in &provider.models {
            if entry.id == model_id {
                return Some((provider.base_url.clone(), entry.model_id.clone()));
            }
        }
    }
    tracing::warn!(model_id = %model_id, "tts_auto_reader: no provider for TTS model");
    None
}

/// Resolve the summary LLM provider + API key.
fn resolve_summary_llm(cfg: &config::AmanConfig, model_id: &str) -> Option<SummaryLlm> {
    for (provider_key, provider) in &cfg.providers {
        for entry in &provider.models {
            if entry.id == model_id {
                let api_key = provider_api_key(provider_key);
                return Some(SummaryLlm {
                    base_url: provider.base_url.clone(),
                    api_model_id: entry.model_id.clone(),
                    api_key,
                });
            }
        }
    }
    tracing::warn!(model_id = %model_id, "tts_auto_reader: no provider for summary model");
    None
}

/// Get a provider's API key (keychain first, then env var fallback).
fn provider_api_key(provider_key: &str) -> Option<String> {
    let use_keyring = config::AmanConfig::from_default_path()
        .map(|cfg| cfg.runtime.security.secrets_mode.use_keyring())
        .unwrap_or(true);
    if use_keyring {
        let backend = secret::KeychainBackend;
        if let Ok(Some(val)) = backend.get(&format!("aman.providers.{provider_key}.api_key")) {
            return Some(val);
        }
    }
    let env_key = format!("AMAN_PROVIDER_{}_API_KEY", provider_key.to_ascii_uppercase());
    std::env::var(env_key).ok()
}

/// Call the summary LLM to produce a one-sentence spoken summary.
async fn llm_summarize(sl: &SummaryLlm, text: &str) -> Result<String, String> {
    let url = format!("{}/chat/completions", sl.base_url.trim_end_matches('/'));

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(SUMMARY_TIMEOUT_SECS))
        .no_proxy()
        .build()
        .map_err(|e| format!("summary http client: {e}"))?;

    let mut builder = client.post(&url).json(&serde_json::json!({
        "model": sl.api_model_id,
        "temperature": 0.3,
        "max_tokens": 128,
        "messages": [
            {
                "role": "system",
                "content": "你是一个摘要助手。用自然口语的一句话概括下文的核心结论。必须压缩到30字以内，不要复述细节，不要解释，只输出这一句话。"
            },
            {"role": "user", "content": text}
        ]
    }));

    if let Some(ref key) = sl.api_key {
        builder = builder.header("Authorization", format!("Bearer {key}"));
    }

    let resp = builder.send().await.map_err(|e| format!("summary request: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("summary API error ({status}): {body}"));
    }

    let v: Value = resp.json().await.map_err(|e| format!("summary decode: {e}"))?;
    let content = v["choices"][0]["message"]["content"]
        .as_str().unwrap_or("").trim().to_owned();
    if content.is_empty() {
        Err("summary returned empty content".to_owned())
    } else {
        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Integration test: resolve TTS provider from real config and synthesize speech.
    ///
    /// Requires omlx (or another TTS provider) to be running locally.
    /// Run manually with:
    /// ```bash
    /// cargo test -p aman-tauri-lib -- --ignored --nocapture tts_synthesis
    /// ```
    #[test]
    #[ignore = "requires local TTS server"]
    #[allow(clippy::print_stderr)] // diagnostic output for manual integration test runs
    fn tts_provider_resolution_and_synthesis() {
        let reader = AutoReader::from_config()
            .expect("TTS config: desktop.auto_read=true, llm.tts set");

        eprintln!(
            "resolved: base_url={}, model={}",
            reader.tts_base_url, reader.api_model_id
        );

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async {
            reader.synthesize("你好，我是aman。这个测试验证TTS自动朗读功能是否正常工作。").await
        });

        match result {
            Ok(audio_bytes) => {
                let path = std::path::PathBuf::from("/tmp/aman_tts_integration_test.wav");
                std::fs::write(&path, &audio_bytes).expect("write audio");
                eprintln!("SUCCESS: {} bytes → {}", audio_bytes.len(), path.display());
            }
            Err(e) => panic!("synthesis failed: {e}"),
        }
    }

    /// Integration test: LLM summarization via `llm.summary`.
    #[test]
    #[ignore = "requires LLM API (deepseek)"]
    #[allow(clippy::print_stderr)] // diagnostic output for manual integration test runs
    fn llm_summary_integration() {
        let reader = AutoReader::from_config()
            .expect("TTS config: desktop.auto_read=true, llm.tts set");

        assert!(reader.summary.is_some(), "llm.summary should be configured");

        let long_reply = "\
            Redis 是一个开源的内存数据结构存储系统。\
            它支持多种数据结构，包括字符串、哈希、列表、集合、有序集合等。\
            Redis 的主要优势在于其极高的读写性能，单机可达十万 QPS。\
            它常用于缓存、会话管理、消息队列、排行榜等场景。\
            此外，Redis 还支持持久化、主从复制和集群模式，可以满足高可用需求。\
            不过需要注意的是，Redis 的数据存储在内存中，成本较高，\
            且默认情况下不保证强一致性。因此通常作为辅助存储，\
            与 PostgreSQL 或 MySQL 等关系型数据库配合使用。";

        let rt = tokio::runtime::Runtime::new().unwrap();
        let summary = rt.block_on(async {
            reader.summarize_reply(long_reply).await
        });

        eprintln!("input_len: {}", long_reply.chars().count());
        eprintln!("summary:   {summary}");
        assert!(!summary.is_empty(), "summary should not be empty");
        // Must be substantially shorter than input (real summarization, not copy).
        assert!(
            summary.chars().count() < long_reply.chars().count() / 2,
            "summary should be much shorter than input: {summary}"
        );
        eprintln!("SUCCESS: {} chars → {} chars", long_reply.chars().count(), summary.chars().count());
    }
}
