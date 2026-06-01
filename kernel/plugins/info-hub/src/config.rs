use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::ai::LlmConfig;

/// Top-level config for the info-hub plugin.
///
/// Accepts two YAML/JSON formats:
/// 1. Object: `{ timeout_ms: 5000, sources: [...], llm: {...} }`
/// 2. Bare array: `[ { name: ..., type: ... }, ... ]` — sources directly,
///    timeout defaults to 10s, no LLM.
#[derive(Debug, Clone, Serialize)]
pub struct InfoHubConfig {
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
    #[serde(default)]
    pub llm: Option<LlmConfig>,
}

fn default_timeout_ms() -> u64 {
    10_000
}

impl Default for InfoHubConfig {
    fn default() -> Self {
        Self {
            timeout_ms: default_timeout_ms(),
            sources: Vec::new(),
            llm: None,
        }
    }
}

impl<'de> serde::Deserialize<'de> for InfoHubConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum RawConfig {
            Object {
                #[serde(default = "default_timeout_ms")]
                timeout_ms: u64,
                #[serde(default)]
                sources: Vec<SourceConfig>,
                #[serde(default)]
                llm: Option<LlmConfig>,
            },
            Array(Vec<SourceConfig>),
        }

        match RawConfig::deserialize(deserializer)? {
            RawConfig::Object {
                timeout_ms,
                sources,
                llm,
            } => Ok(Self {
                timeout_ms,
                sources,
                llm,
            }),
            RawConfig::Array(sources) => Ok(Self {
                timeout_ms: default_timeout_ms(),
                sources,
                llm: None,
            }),
        }
    }
}

/// A single data source configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceConfig {
    Api {
        name: String,
        api_url: String,
        #[serde(default)]
        api_key: Option<String>,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
    Cli {
        name: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Db {
        name: String,
        runtime: String,
        script: String,
        #[serde(default)]
        db_path: Option<String>,
    },
    Embedding {
        name: String,
        /// OpenAI-compatible embeddings endpoint (e.g. http://127.0.0.1:11434/v1).
        base_url: String,
        /// Embedding model name (e.g. qwen3-embedding-8b).
        model: String,
        #[serde(default)]
        api_key: Option<String>,
        /// Path to SQLite database containing articles for the candidate pool.
        db_path: String,
        /// Minimum cosine similarity (0.0–1.0). Results below this are dropped.
        #[serde(default = "default_embedding_threshold")]
        threshold: f64,
        /// Max number of candidate articles to fetch from the DB for embedding.
        #[serde(default = "default_max_candidates")]
        max_candidates: usize,
    },
}

fn default_embedding_threshold() -> f64 { 0.5 }
fn default_max_candidates() -> usize { 50 }

impl SourceConfig {
    pub fn name(&self) -> &str {
        match self {
            SourceConfig::Api { name, .. }
            | SourceConfig::Cli { name, .. }
            | SourceConfig::Db { name, .. }
            | SourceConfig::Embedding { name, .. } => name,
        }
    }
}

impl InfoHubConfig {
    /// Parse from a YAML string containing the `info_hub:` block value.
    pub fn from_yaml_str(yaml: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Filter sources by an optional allowlist. Returns all sources if allowlist is empty.
    pub fn filter_sources(&self, allowlist: &[String]) -> Vec<&SourceConfig> {
        if allowlist.is_empty() {
            self.sources.iter().collect()
        } else {
            self.sources
                .iter()
                .filter(|s| allowlist.iter().any(|name| name == s.name()))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config() {
        let yaml = r#"
timeout_ms: 5000
sources:
  - name: rsshub-tech
    type: api
    api_url: "https://rsshub.app/feed/{query}"
    api_key: "Bearer ${RSSHUB_TOKEN}"
  - name: blogwatcher
    type: cli
    command: blogwatcher
    args: ["search", "{query}", "--json"]
  - name: fusion-local
    type: db
    runtime: python3
    script: ~/.aman/scripts/fusion_search.py
    db_path: ~/.fusion/data.db
"#;
        let config: InfoHubConfig = serde_yaml::from_str(yaml).expect("parse config");
        assert_eq!(config.timeout_ms, 5000);
        assert_eq!(config.sources.len(), 3);
        assert_eq!(config.sources[0].name(), "rsshub-tech");
        assert_eq!(config.sources[1].name(), "blogwatcher");
        assert_eq!(config.sources[2].name(), "fusion-local");
    }

    #[test]
    fn filter_sources_respects_allowlist() {
        let config = InfoHubConfig {
            sources: vec![
                SourceConfig::Cli {
                    name: "a".into(),
                    command: "cmd-a".into(),
                    args: vec![],
                },
                SourceConfig::Cli {
                    name: "b".into(),
                    command: "cmd-b".into(),
                    args: vec![],
                },
            ],
            ..Default::default()
        };
        let filtered = config.filter_sources(&["a".into()]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name(), "a");
    }

    #[test]
    fn parse_env_var_in_api_key() {
        let yaml = r#"
sources:
  - name: test
    type: api
    api_url: "https://example.com/{query}"
    api_key: "Bearer ${MY_TOKEN}"
"#;
        let config: InfoHubConfig = serde_yaml::from_str(yaml).expect("parse");
        let SourceConfig::Api { api_key, .. } = &config.sources[0] else {
            panic!("expected api source");
        };
        assert_eq!(api_key.as_deref(), Some("Bearer ${MY_TOKEN}"));
    }
}
