pub mod adapters;
pub mod ai;
pub mod config;
pub mod merge;
pub mod types;

use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use kernel::context::{PluginContext, ToolContext};
use kernel::plugin::{Plugin, PluginDependency};
use kernel::schema::JsonSchema;
use kernel::source::EventSource;
use kernel::tool::{Tool, ToolResult};
use kernel::types::ToolMode;
use kernel::AmanResult;
use semver::Version;
use serde_json::{json, Value};
use tracing::warn;

use cognitive_llm::provider::ResponseFormat;

use ai::{
    ArticleInput, LlmConfig, ScoreResult, SummaryResult, TagResult,
    build_highlights_prompt, build_scoring_prompt, build_summary_prompt,
    build_tagging_prompt, chat_completion_with_retries, parse_json_response,
    truncate_str,
};
use config::InfoHubConfig;
use types::InfoSearchInput;

const TAGGING_TEMPERATURE: f64 = 0.265;
const SCORING_TEMPERATURE: f64 = 0.377;
const SUMMARIZING_TEMPERATURE: f64 = 0.465;
const HIGHLIGHTS_TEMPERATURE: f64 = 0.578;

// JSON schemas for structured output (json_schema mode).
static TAGGING_SCHEMA: LazyLock<ResponseFormat> = LazyLock::new(|| {
    ResponseFormat::JsonSchema {
        name: "tagging".into(),
        strict: true,
        schema: json!({
            "type": "object",
            "properties": {
                "results": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "index": { "type": "integer" },
                            "category": { "type": "string" },
                            "keywords": {
                                "type": "array",
                                "items": { "type": "string" }
                            }
                        },
                        "required": ["index", "category", "keywords"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["results"],
            "additionalProperties": false
        }),
    }
});

static SCORING_SCHEMA: LazyLock<ResponseFormat> = LazyLock::new(|| {
    ResponseFormat::JsonSchema {
        name: "scoring".into(),
        strict: true,
        schema: json!({
            "type": "object",
            "properties": {
                "results": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "index": { "type": "integer" },
                            "relevance": { "type": "integer" },
                            "quality": { "type": "integer" },
                            "timeliness": { "type": "integer" }
                        },
                        "required": ["index", "relevance", "quality", "timeliness"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["results"],
            "additionalProperties": false
        }),
    }
});

static SUMMARY_SCHEMA: LazyLock<ResponseFormat> = LazyLock::new(|| {
    ResponseFormat::JsonSchema {
        name: "summary".into(),
        strict: true,
        schema: json!({
            "type": "object",
            "properties": {
                "results": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "index": { "type": "integer" },
                            "title_zh": { "type": "string" },
                            "summary": { "type": "string" },
                            "reason": { "type": "string" }
                        },
                        "required": ["index", "title_zh", "summary", "reason"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["results"],
            "additionalProperties": false
        }),
    }
});

// ── info_search ───────────────────────────────────────────────────────

struct InfoSearchTool {
    config: InfoHubConfig,
}

impl InfoSearchTool {
    fn new(config: InfoHubConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Tool for InfoSearchTool {
    fn name(&self) -> &str {
        "info_search"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "Search across configured RSS, CLI tools, and local databases. Query is free-text."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Free-text search query"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results (default 20)",
                        "default": 20
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Result offset for pagination",
                        "default": 0
                    },
                    "sources": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional source names to search (empty = all)"
                    }
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": {"type": "string"},
                        "url": {"type": "string"},
                        "summary": {"type": "string"},
                        "published": {"type": "string"},
                        "source": {"type": "string"}
                    }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> ToolResult {
        let input: InfoSearchInput = serde_json::from_value(params).map_err(|e| {
            kernel::Error::config_invalid(format!("info_search params: {e}"))
        })?;

        let allowed_sources: Vec<String> =
            input.sources.clone().unwrap_or_default();
        let sources = self.config.filter_sources(&allowed_sources);

        if sources.is_empty() {
            return Ok(json!([]));
        }

        let mut tasks = Vec::new();
        for source in sources {
            let adapter = adapters::build_adapter(source, self.config.timeout_ms);
            let input = InfoSearchInput {
                query: input.query.clone(),
                limit: input.limit,
                offset: input.offset,
                sources: Some(vec![source.name().to_string()]),
            };
            tasks.push(tokio::spawn(async move {
                adapter.search(&input).await
            }));
        }

        let mut all_items = Vec::new();
        for task in tasks {
            match task.await {
                Ok(Ok(items)) => all_items.extend(items),
                Ok(Err(e)) => warn!(%e, "info-hub adapter search failed"),
                Err(e) => warn!(%e, "info-hub adapter task panicked"),
            }
        }

        let merged = merge::merge(all_items, input.limit);
        Ok(serde_json::to_value(merged).unwrap_or(json!([])))
    }
}

// ── info_tag_articles ─────────────────────────────────────────────────

struct InfoTagArticlesTool {
    llm: Option<LlmConfig>,
}

#[async_trait]
impl Tool for InfoTagArticlesTool {
    fn name(&self) -> &str {
        "info_tag_articles"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "Tag articles with category/domain label and extract keywords. Requires LLM."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["articles"],
                "properties": {
                    "articles": {
                        "type": "array",
                        "description": "Articles to tag",
                        "items": {
                            "type": "object",
                            "required": ["index", "title", "description"],
                            "properties": {
                                "index": {"type": "integer"},
                                "title": {"type": "string"},
                                "description": {"type": "string"},
                                "source_name": {"type": "string"},
                                "link": {"type": "string"}
                            }
                        }
                    }
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "results": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "index": {"type": "integer"},
                                "category": {"type": "string"},
                                "keywords": {"type": "array", "items": {"type": "string"}}
                            }
                        }
                    }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> ToolResult {
        let llm = self.llm.as_ref().ok_or_else(|| {
            kernel::Error::config_invalid("info_tag_articles: no LLM configured")
        })?;

        let articles: Vec<ArticleInput> =
            serde_json::from_value(params.get("articles").cloned().unwrap_or(json!([])))
                .map_err(|e| kernel::Error::config_invalid(format!("articles parse: {e}")))?;

        let (system, user) = build_tagging_prompt(&articles);
        let text = chat_completion_with_retries(llm, &system, &user, TAGGING_TEMPERATURE, 2048, 60, 3, Some(&TAGGING_SCHEMA)).await
            .map_err(|e| kernel::Error::Unrecoverable { message: format!("LLM: {e}") })?;

        let results: Vec<TagResult> = parse_json_response::<Value>(&text)
            .ok()
            .and_then(|v| v.pointer("/results").cloned())
            .and_then(|r| serde_json::from_value(r).ok())
            .unwrap_or_default();

        // Enforce max 3 keywords per article
        let results: Vec<TagResult> = results
            .into_iter()
            .map(|mut r| {
                r.keywords.truncate(3);
                r
            })
            .collect();

        Ok(serde_json::to_value(json!({"results": results})).unwrap())
    }
}

// ── info_score_articles ───────────────────────────────────────────────

struct InfoScoreArticlesTool {
    llm: Option<LlmConfig>,
}

#[async_trait]
impl Tool for InfoScoreArticlesTool {
    fn name(&self) -> &str {
        "info_score_articles"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "Score articles on relevance, quality, and timeliness (1-10). Requires LLM."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["articles"],
                "properties": {
                    "articles": {
                        "type": "array",
                        "description": "Articles to score, optionally with tags from info_tag_articles",
                        "items": {
                            "type": "object",
                            "required": ["index", "title", "description"],
                            "properties": {
                                "index": {"type": "integer"},
                                "title": {"type": "string"},
                                "description": {"type": "string"},
                                "source_name": {"type": "string"},
                                "link": {"type": "string"},
                                "category": {"type": "string", "description": "Pre-assigned category from tagging step"},
                                "keywords": {"type": "array", "items": {"type": "string"}, "description": "Pre-assigned keywords from tagging step"}
                            }
                        }
                    }
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "results": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "index": {"type": "integer"},
                                "relevance": {"type": "integer"},
                                "quality": {"type": "integer"},
                                "timeliness": {"type": "integer"}
                            }
                        }
                    }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> ToolResult {
        let llm = self.llm.as_ref().ok_or_else(|| {
            kernel::Error::config_invalid("info_score_articles: no LLM configured")
        })?;

        let articles: Vec<ArticleInput> =
            serde_json::from_value(params.get("articles").cloned().unwrap_or(json!([])))
                .map_err(|e| kernel::Error::config_invalid(format!("articles parse: {e}")))?;

        let (system, user) = build_scoring_prompt(&articles);
        let text = chat_completion_with_retries(llm, &system, &user, SCORING_TEMPERATURE, 4096, 60, 3, Some(&SCORING_SCHEMA)).await
            .map_err(|e| kernel::Error::Unrecoverable { message: format!("LLM: {e}") })?;

        let results: Vec<ScoreResult> = parse_json_response::<Value>(&text)
            .ok()
            .and_then(|v| v.pointer("/results").cloned())
            .and_then(|r| serde_json::from_value(r).ok())
            .unwrap_or_default();

        Ok(serde_json::to_value(json!({"results": results})).unwrap())
    }
}

// ── info_summarize_articles ───────────────────────────────────────────

struct InfoSummarizeArticlesTool {
    llm: Option<LlmConfig>,
}

#[async_trait]
impl Tool for InfoSummarizeArticlesTool {
    fn name(&self) -> &str {
        "info_summarize_articles"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "Summarize articles with Chinese title translation, structured summary, and reading recommendation. Requires LLM."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["articles"],
                "properties": {
                    "articles": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "required": ["index", "title", "description"],
                            "properties": {
                                "index": {"type": "integer"},
                                "title": {"type": "string"},
                                "description": {"type": "string"},
                                "source_name": {"type": "string"},
                                "link": {"type": "string"},
                                "relevance": {"type": "integer", "description": "Relevance score 1-10 from prior scoring"},
                                "quality": {"type": "integer", "description": "Quality score 1-10 from prior scoring"},
                                "timeliness": {"type": "integer", "description": "Timeliness score 1-10 from prior scoring"}
                            }
                        }
                    },
                    "lang": {
                        "type": "string",
                        "description": "Output language (zh or en, default zh)",
                        "default": "zh"
                    },
                    "min_score": {
                        "type": "integer",
                        "description": "Minimum total score (relevance+quality+timeliness, 3-30) to summarize. Articles below this get fallback. Default 0 (no filter).",
                        "default": 0
                    }
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "properties": {
                    "results": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "index": {"type": "integer"},
                                "title_zh": {"type": "string"},
                                "summary": {"type": "string"},
                                "reason": {"type": "string"}
                            }
                        }
                    }
                }
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> ToolResult {
        let llm = self.llm.as_ref().ok_or_else(|| {
            kernel::Error::config_invalid("info_summarize_articles: no LLM configured")
        })?;

        let articles: Vec<ArticleInput> =
            serde_json::from_value(params.get("articles").cloned().unwrap_or(json!([])))
                .map_err(|e| kernel::Error::config_invalid(format!("articles parse: {e}")))?;

        let lang = params
            .get("lang")
            .and_then(|v| v.as_str())
            .unwrap_or("zh");

        let min_score: u32 = params
            .get("min_score")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        // Split: summarize articles meeting threshold, fallback for the rest
        let (to_summarize, skipped): (Vec<&ArticleInput>, Vec<&ArticleInput>) =
            articles.iter().partition(|a| min_score == 0 || a.total_score() >= min_score);

        let mut results: Vec<SummaryResult> = Vec::with_capacity(articles.len());

        // Fallback entries first (no LLM call needed)
        for a in &skipped {
            results.push(SummaryResult {
                index: a.index,
                title_zh: a.title.clone(),
                summary: format!(
                    "[skipped: score {}/30 below threshold] {}",
                    a.total_score(),
                    truncate_str(&a.description, 120),
                ),
                reason: String::new(),
            });
        }

        // Only call LLM if there are articles worth summarizing
        if !to_summarize.is_empty() {
            let articles_refs: Vec<ArticleInput> = to_summarize.iter().map(|&a| a.clone()).collect();
            let (system, user) = build_summary_prompt(&articles_refs, lang);
            let text = chat_completion_with_retries(llm, &system, &user, SUMMARIZING_TEMPERATURE, 8192, 60, 3, Some(&SUMMARY_SCHEMA)).await
                .map_err(|e| kernel::Error::Unrecoverable { message: format!("LLM: {e}") })?;

            let llm_results: Vec<SummaryResult> = parse_json_response::<Value>(&text)
                .ok()
                .and_then(|v| v.pointer("/results").cloned())
                .and_then(|r| serde_json::from_value(r).ok())
                .unwrap_or_default();

            results.extend(llm_results);
        }

        // Sort by original index before returning
        results.sort_by_key(|r| r.index);

        Ok(serde_json::to_value(json!({"results": results})).unwrap())
    }
}

// ── info_generate_highlights ──────────────────────────────────────────

struct InfoGenerateHighlightsTool {
    llm: Option<LlmConfig>,
}

#[async_trait]
impl Tool for InfoGenerateHighlightsTool {
    fn name(&self) -> &str {
        "info_generate_highlights"
    }

    fn mode(&self) -> ToolMode {
        ToolMode::Local
    }

    fn description(&self) -> &str {
        "Generate a 3-5 sentence trend overview from a list of today's top articles. Requires LLM."
    }

    fn parameters(&self) -> &JsonSchema {
        static PARAMS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "object",
                "required": ["articles_json"],
                "properties": {
                    "articles_json": {
                        "type": "string",
                        "description": "JSON string of the article list"
                    },
                    "lang": {
                        "type": "string",
                        "description": "Output language (zh or en, default zh)",
                        "default": "zh"
                    }
                }
            }))
        });
        &PARAMS
    }

    fn returns(&self) -> &JsonSchema {
        static RETURNS: LazyLock<JsonSchema> = LazyLock::new(|| {
            JsonSchema::from(json!({
                "type": "string",
                "description": "Plain text trend overview"
            }))
        });
        &RETURNS
    }

    async fn execute(&self, params: Value, _ctx: ToolContext) -> ToolResult {
        let llm = self.llm.as_ref().ok_or_else(|| {
            kernel::Error::config_invalid("info_generate_highlights: no LLM configured")
        })?;

        let articles_json = params
            .get("articles_json")
            .and_then(|v| v.as_str())
            .unwrap_or("[]");

        let lang = params
            .get("lang")
            .and_then(|v| v.as_str())
            .unwrap_or("zh");

        let (system, user) = build_highlights_prompt(articles_json, lang);
        let text = chat_completion_with_retries(llm, &system, &user, HIGHLIGHTS_TEMPERATURE, 2048, 60, 3, None).await
            .map_err(|e| kernel::Error::Unrecoverable { message: format!("LLM: {e}") })?;

        Ok(Value::String(text.trim().to_string()))
    }
}

// ── Plugin ────────────────────────────────────────────────────────────

pub struct InfoHubPlugin {
    version: Version,
    config: InfoHubConfig,
}

impl InfoHubPlugin {
    pub fn new(config: InfoHubConfig) -> Self {
        Self {
            version: Version::new(0, 1, 0),
            config,
        }
    }

    /// Create from the raw `info_hub` value in AmanConfig.
    pub fn from_config_value(value: Option<&Value>) -> Self {
        let config = value
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        Self::new(config)
    }

    /// Resolve LLM config from `memory.llm` + `providers` and return a plugin ready to use.
    /// `llm_cfg` should be the resolved `LlmConfig` (base_url, api_key, model).
    /// Falls back to `info_hub.llm` if set directly in the info-hub config.
    pub fn from_config_with_llm(value: Option<&Value>, resolved_llm: Option<LlmConfig>) -> Self {
        let mut config: InfoHubConfig = value
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        if config.llm.is_none() {
            config.llm = resolved_llm;
        }
        Self::new(config)
    }
}

#[async_trait]
impl Plugin for InfoHubPlugin {
    fn name(&self) -> &str {
        "info-hub"
    }

    fn version(&self) -> &Version {
        &self.version
    }

    fn dependencies(&self) -> &[PluginDependency] {
        &[]
    }

    async fn on_load(&mut self, _ctx: PluginContext) -> AmanResult<()> {
        // Initialize the Python prompt bridge so all LLM prompt text comes
        // from prompts.py, not hardcoded Rust strings.
        ai::init_prompt_bridge(
            self.config.python_runtime.clone(),
            self.config.prompts_script.clone(),
        );
        Ok(())
    }

    async fn on_unload(&mut self) -> AmanResult<()> {
        Ok(())
    }

    async fn on_dependency_unloading(&self, _dep_name: &str) -> AmanResult<()> {
        Ok(())
    }

    fn event_sources(&self) -> Vec<Arc<dyn EventSource>> {
        vec![]
    }

    fn skills(&self) -> Vec<Arc<dyn kernel::skill::Skill>> {
        vec![]
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let llm = self.config.llm.clone();
        vec![
            Arc::new(InfoSearchTool::new(self.config.clone())),
            Arc::new(InfoTagArticlesTool { llm: llm.clone() }),
            Arc::new(InfoScoreArticlesTool { llm: llm.clone() }),
            Arc::new(InfoSummarizeArticlesTool { llm: llm.clone() }),
            Arc::new(InfoGenerateHighlightsTool { llm }),
        ]
    }
}
