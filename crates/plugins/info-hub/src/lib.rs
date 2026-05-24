mod adapters;
pub mod config;
mod merge;
pub mod types;

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
use std::sync::{Arc, LazyLock};
use tracing::warn;

use config::InfoHubConfig;
use types::InfoSearchInput;

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

        // Build adapters and run searches in parallel
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

pub struct InfoHubPlugin {
    version: Version,
    config: InfoHubConfig,
}

impl InfoHubPlugin {
    /// Create from a parsed `InfoHubConfig`.
    pub fn new(config: InfoHubConfig) -> Self {
        Self {
            version: Version::new(0, 1, 0),
            config,
        }
    }

    /// Create from the raw `info_hub` value in AmanConfig.
    /// Returns a plugin with empty source list if the value is absent or invalid.
    pub fn from_config_value(value: Option<&Value>) -> Self {
        let config = value
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
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
        vec![Arc::new(InfoSearchTool::new(self.config.clone()))]
    }
}
