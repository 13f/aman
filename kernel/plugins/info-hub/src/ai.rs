//! AI processing: scoring, summarization, and highlights generation.
//!
//! Uses the LLM configured via `memory.llm` in aman config. Makes
//! OpenAI-compatible chat completion calls via `cognitive_llm`. No provider-specific logic.
//!
//! All prompt text lives in `predefined/plugins/info-hub/prompts.py` —
//! no hardcoded prompt strings in Rust.

use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::Value;
use std::process::Command;
use std::sync::OnceLock;
use tracing::{debug, warn};

// Re-export LLM API primitives from the shared crate.
pub use cognitive_llm::simple::LlmApiConfig as LlmConfig;
pub use cognitive_llm::simple::parse_json_response;
use cognitive_llm::simple::SimpleLlmClient;

#[allow(dead_code)]
const DESCRIPTION_MAX_LEN: usize = 384;

// ── Prompt bridge (calls prompts.py instead of hardcoding) ──────────────

/// Global bridge config — set once during plugin init.
static PROMPT_BRIDGE: OnceLock<PromptBridge> = OnceLock::new();

struct PromptBridge {
    python: String,
    script: String,
}

impl PromptBridge {
    fn call(&self, method: &str, args: &serde_json::Value) -> Option<(String, String)> {
        let args_str = args.to_string();
        let result = Command::new(&self.python)
            .arg(&self.script)
            .arg(method)
            .arg(&args_str)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output();

        match result {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let parsed: serde_json::Value = serde_json::from_str(&stdout).ok()?;
                let system = parsed.get("system")?.as_str()?.to_owned();
                let user = parsed.get("user")?.as_str()?.to_owned();
                debug!(method, "prompts.py call succeeded");
                Some((system, user))
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(
                    method,
                    stderr = %stderr.trim(),
                    "prompts.py call failed, using minimal fallback"
                );
                None
            }
            Err(e) => {
                warn!(
                    method,
                    error = %e,
                    "failed to spawn prompts.py, using minimal fallback"
                );
                None
            }
        }
    }
}

/// Initialize the prompt bridge. Called once during plugin startup.
pub fn init_prompt_bridge(python: String, script: String) {
    let bridge = PromptBridge { python, script };
    let _ = PROMPT_BRIDGE.set(bridge);
}

/// Call the Python prompts module. Falls back to an empty system prompt on failure.
fn build_prompt(method: &str, args: &serde_json::Value) -> (String, String) {
    if let Some(bridge) = PROMPT_BRIDGE.get()
        && let Some((system, user)) = bridge.call(method, args)
    {
        return (system, user);
    }
    // Minimal fallback — the LLM can still infer the task from the user message format.
    warn!(method, "PromptBridge unavailable, using empty system prompt");
    (String::new(), String::new())
}

// ── Types ───────────────────────────────────────────────────────────

/// One article sent to the scoring/summarization tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArticleInput {
    pub index: usize,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub source_name: String,
    #[serde(default)]
    pub link: String,
    /// Pre-assigned category from tagging step (used by scoring prompt for context).
    #[serde(default)]
    pub category: String,
    /// Pre-assigned keywords from tagging step (used by scoring prompt for context).
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Score from prior scoring step (used by summarizer to skip low-score articles).
    #[serde(default)]
    pub relevance: u32,
    #[serde(default)]
    pub quality: u32,
    #[serde(default)]
    pub timeliness: u32,
}

impl ArticleInput {
    pub fn total_score(&self) -> u32 {
        self.relevance + self.quality + self.timeliness
    }
}

/// Score result for a single article.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreResult {
    pub index: usize,
    pub relevance: u32,
    pub quality: u32,
    pub timeliness: u32,
}

/// Tag result for a single article (category + keywords only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagResult {
    pub index: usize,
    pub category: String,
    pub keywords: Vec<String>,
}

/// Summary result for a single article.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryResult {
    pub index: usize,
    pub title_zh: String,
    pub summary: String,
    pub reason: String,
}

// ── LLM Client (delegates to cognitive-llm) ─────────────────────────

/// One-shot chat completion via the shared `SimpleLlmClient`.
pub async fn chat_completion(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f64,
    max_tokens: u64,
    timeout_secs: u64,
    response_format: Option<&cognitive_llm::provider::ResponseFormat>,
) -> Result<String, String> {
    SimpleLlmClient::new().chat_completion(config, system_prompt, user_prompt, temperature, max_tokens, timeout_secs, response_format).await
}

/// Chat completion with retries via the shared `SimpleLlmClient`.
#[allow(clippy::too_many_arguments)] // thin wrapper delegating to SimpleLlmClient
pub async fn chat_completion_with_retries(
    config: &LlmConfig,
    system_prompt: &str,
    user_prompt: &str,
    temperature: f64,
    max_tokens: u64,
    timeout_secs: u64,
    retries: u32,
    response_format: Option<&cognitive_llm::provider::ResponseFormat>,
) -> Result<String, String> {
    SimpleLlmClient::new().chat_completion_with_retries(config, system_prompt, user_prompt, temperature, max_tokens, timeout_secs, retries, response_format).await
}

// ── Prompt Templates ────────────────────────────────────────────────

pub fn build_scoring_prompt(
    articles: &[ArticleInput],
) -> (String, String) {
    let articles_json: Vec<serde_json::Value> = articles
        .iter()
        .map(|a| serde_json::to_value(a).unwrap_or_default())
        .collect();
    let args = serde_json::json!({"articles": articles_json});
    build_prompt("scoring", &args)
}

pub fn build_tagging_prompt(
    articles: &[ArticleInput],
) -> (String, String) {
    let articles_json: Vec<serde_json::Value> = articles
        .iter()
        .map(|a| serde_json::to_value(a).unwrap_or_default())
        .collect();
    let args = serde_json::json!({"articles": articles_json});
    build_prompt("tagging", &args)
}

pub fn build_summary_prompt(
    articles: &[ArticleInput],
    lang: &str,
) -> (String, String) {
    let articles_json: Vec<serde_json::Value> = articles
        .iter()
        .map(|a| serde_json::to_value(a).unwrap_or_default())
        .collect();
    let args = serde_json::json!({"articles": articles_json, "lang": lang});
    build_prompt("summary", &args)
}

pub fn build_highlights_prompt(
    articles_json: &str,
    lang: &str,
) -> (String, String) {
    let args = serde_json::json!({"articles_json": articles_json, "lang": lang});
    build_prompt("highlights", &args)
}

// ── Helpers ─────────────────────────────────────────────────────────

#[allow(dead_code)]
fn truncate_description(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let sliced = &text[..max_len];
    // Try to break at a sentence boundary
    if let Some(pos) = sliced.rfind(['.', '!', '?', '。', '！', '？']) {
        return sliced[..=pos].trim_end().to_string();
    }
    // Fallback to last space
    if let Some(pos) = sliced.rfind(' ')
        && pos > max_len * 3 / 5
    {
        return sliced[..pos].to_string();
    }
    sliced.to_string()
}

/// Process articles in batches with bounded concurrency.
pub async fn process_batches<T, F, R>(
    items: &[T],
    batch_size: usize,
    max_concurrent: usize,
    f: F,
) -> Vec<R>
where
    T: Send + Sync,
    F: Fn(Vec<&T>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<R>> + Send>> + Send + Sync + 'static,
    R: Send + 'static,
{
    let mut results: Vec<R> = Vec::new();
    let batches: Vec<Vec<&T>> = items
        .chunks(batch_size)
        .map(|c| c.iter().collect())
        .collect();

    debug!(items = items.len(), batches = batches.len(), "info-hub batch processing");

    for batch_group in batches.chunks(max_concurrent) {
        let tasks: Vec<_> = batch_group
            .iter()
            .map(|batch| {
                let batch: Vec<&T> = batch.to_vec();
                f(batch)
            })
            .collect();

        let group_results = futures::future::join_all(tasks).await;
        for r in group_results {
            results.extend(r);
        }
    }

    results
}

/// Clamp a score to 1..=10
pub fn clamp_score(v: i64) -> u32 {
    v.clamp(1, 10) as u32
}

/// Simple truncation helper (public for use in tool fallback messages).
pub fn truncate_str(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let sliced = &text[..max_len];
    if let Some(pos) = sliced.rfind(['.', '!', '?', '。', '！', '？']) {
        return sliced[..=pos].trim_end().to_string();
    }
    if let Some(pos) = sliced.rfind(' ') {
        return sliced[..pos].to_string();
    }
    sliced.to_string()
}

pub const VALID_CATEGORIES: &[&str] = &[
    "ai-ml", "security", "engineering", "tools", "opinion", "other",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_limit() {
        let short = "hello";
        assert_eq!(truncate_description(short, 20), "hello");
        let long = "a".repeat(100);
        assert!(truncate_description(&long, 50).len() <= 50);
    }

    #[test]
    fn truncate_breaks_at_sentence() {
        let text = "First sentence. Second sentence that is very long.";
        let truncated = truncate_description(text, 30);
        assert!(truncated.ends_with('.'));
        assert!(!truncated.contains("Second"));
    }

    #[test]
    fn parse_json_extracts_from_markdown_fence() {
        let raw = "```json\n{\"key\": \"value\"}\n```";
        let parsed: Value = parse_json_response(raw).unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn parse_json_repairs_truncated() {
        let raw = "{\"results\": [{\"index\": 0}";
        let parsed: Value = parse_json_response(raw).unwrap();
        assert_eq!(parsed["results"][0]["index"], 0);
    }

    #[test]
    fn clamp_score_bounds() {
        assert_eq!(clamp_score(0), 1);
        assert_eq!(clamp_score(5), 5);
        assert_eq!(clamp_score(11), 10);
    }
}
