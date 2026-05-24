use serde::{Deserialize, Serialize};

/// Normalized search result from any data source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfoItem {
    pub title: String,
    pub url: String,
    pub summary: String,
    pub published: Option<String>,
    pub source: String,
    #[serde(default)]
    pub raw: serde_json::Value,
}

/// Input to the info_search tool, deserialized from LLM-generated JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfoSearchInput {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub sources: Option<Vec<String>>,
}

const fn default_limit() -> usize {
    20
}
