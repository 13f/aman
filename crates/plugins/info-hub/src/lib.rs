mod adapters;
pub mod ai;
pub mod config;
mod merge;
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

use ai::{
    ArticleInput, LlmConfig, ScoreResult, SummaryResult, TagResult,
    build_highlights_prompt, build_scoring_prompt, build_summary_prompt,
    build_tagging_prompt, chat_completion_with_retries, parse_json_response,
};
use config::InfoHubConfig;
use types::InfoSearchInput;

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
        let text = chat_completion_with_retries(llm, &system, &user, 0.2, 2048, 60, 3).await
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
        let text = chat_completion_with_retries(llm, &system, &user, 0.3, 4096, 60, 3).await
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
                                "link": {"type": "string"}
                            }
                        }
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

        let (system, user) = build_summary_prompt(&articles, lang);
        let text = chat_completion_with_retries(llm, &system, &user, 0.4, 8192, 60, 3).await
            .map_err(|e| kernel::Error::Unrecoverable { message: format!("LLM: {e}") })?;

        let results: Vec<SummaryResult> = parse_json_response::<Value>(&text)
            .ok()
            .and_then(|v| v.pointer("/results").cloned())
            .and_then(|r| serde_json::from_value(r).ok())
            .unwrap_or_default();

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
        let text = chat_completion_with_retries(llm, &system, &user, 0.5, 2048, 60, 3).await
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
