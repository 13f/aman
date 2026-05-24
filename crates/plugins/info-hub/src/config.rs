use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Top-level config for the info-hub plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfoHubConfig {
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    pub sources: Vec<SourceConfig>,
}

fn default_timeout_ms() -> u64 {
    10_000
}

impl Default for InfoHubConfig {
    fn default() -> Self {
        Self {
            timeout_ms: default_timeout_ms(),
            sources: Vec::new(),
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
}

impl SourceConfig {
    pub fn name(&self) -> &str {
        match self {
            SourceConfig::Api { name, .. }
            | SourceConfig::Cli { name, .. }
            | SourceConfig::Db { name, .. } => name,
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
