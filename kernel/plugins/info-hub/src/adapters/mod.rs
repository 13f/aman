use async_trait::async_trait;
use kernel::AmanResult;

use crate::config::SourceConfig;
use crate::types::{InfoItem, InfoSearchInput};

pub mod api;
pub mod cli;
pub mod db;
pub mod embedding;

/// A single data-source adapter. Each variant knows how to query one kind of source.
#[async_trait]
pub trait Adapter: Send + Sync {
    /// Execute a search against this adapter's source and return normalized results.
    async fn search(&self, input: &InfoSearchInput) -> AmanResult<Vec<InfoItem>>;
}

/// Expand `~` at the start of a path to the user's home directory.
pub fn expand_tilde(path: &str) -> String {
    if path.starts_with('~') && let Ok(home) = std::env::var("HOME") {
        if path == "~" {
            return home;
        }
        if let Some(rest) = path.strip_prefix("~/") {
            return format!("{home}/{rest}");
        }
    }
    path.to_string()
}

/// Build the appropriate adapter for a source config.
pub fn build_adapter(source: &SourceConfig, timeout_ms: u64) -> Box<dyn Adapter> {
    let name = source.name().to_string();
    match source {
        SourceConfig::Api {
            api_url,
            api_key,
            headers,
            ..
        } => Box::new(api::ApiAdapter::new(
            name,
            api_url.clone(),
            api_key.clone(),
            headers.clone(),
            timeout_ms,
        )),
        SourceConfig::Cli { command, args, .. } => {
            Box::new(cli::CliAdapter::new(
                name,
                command.clone(),
                args.clone(),
                timeout_ms,
            ))
        }
        SourceConfig::Db {
            runtime,
            script,
            db_path,
            ..
        } => Box::new(db::DbAdapter::new(
            name,
            runtime.clone(),
            script.clone(),
            db_path.clone(),
            timeout_ms,
        )),
        SourceConfig::Embedding {
            base_url,
            model,
            api_key,
            db_path,
            threshold,
            max_candidates,
            ..
        } => Box::new(embedding::EmbeddingAdapter::new(
            name,
            base_url.clone(),
            model.clone(),
            api_key.clone(),
            db_path.clone(),
            *threshold,
            *max_candidates,
            timeout_ms,
        )),
    }
}

/// Resolve `${VAR}` references in a string from environment variables.
pub fn resolve_env(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next(); // skip '{'
            let mut var = String::new();
            for ch in chars.by_ref() {
                if ch == '}' {
                    break;
                }
                var.push(ch);
            }
            let value = std::env::var(&var).unwrap_or_default();
            result.push_str(&value);
        } else {
            result.push(c);
        }
    }
    result
}

/// Replace placeholder tokens in a template string.
/// Supported: `{query}`, `{limit}`, `{offset}`
pub fn replace_placeholders(template: &str, input: &InfoSearchInput) -> String {
    template
        .replace("{query}", &input.query)
        .replace("{limit}", &input.limit.to_string())
        .replace("{offset}", &input.offset.to_string())
}

/// Try to interpret an arbitrary JSON value as a list of InfoItems.
/// Accepts: JSON array of objects, or object with "items"/"results"/"data" array field.
pub fn normalize_json_items(
    value: &serde_json::Value,
    source_name: &str,
) -> Vec<InfoItem> {
    let items = match value {
        serde_json::Value::Array(arr) => arr.clone(),
        serde_json::Value::Object(map) => {
            map.get("items")
                .or_else(|| map.get("results"))
                .or_else(|| map.get("data"))
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
        }
        _ => return Vec::new(),
    };

    items
        .into_iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            Some(InfoItem {
                title: obj
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                url: obj
                    .get("url")
                    .or_else(|| obj.get("link"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                summary: obj
                    .get("summary")
                    .or_else(|| obj.get("description"))
                    .or_else(|| obj.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                published: obj
                    .get("published")
                    .or_else(|| obj.get("pub_date"))
                    .or_else(|| obj.get("date"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                source: source_name.to_string(),
                raw: serde_json::Value::Object(obj.clone()),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_env_replaces_var() {
        // Use HOME which is always set in POSIX environments.
        let home = std::env::var("HOME").unwrap_or_default();
        let result = resolve_env("prefix-${HOME}-suffix");
        assert_eq!(result, format!("prefix-{home}-suffix"));
    }

    #[test]
    fn resolve_env_missing_var_is_empty() {
        assert_eq!(resolve_env("${NO_SUCH_VAR_XYZ_123}"), "");
    }

    #[test]
    fn replace_placeholders_fills_all() {
        let input = InfoSearchInput {
            query: "rust".into(),
            limit: 10,
            offset: 5,
            sources: None,
        };
        let result = replace_placeholders("{query} limit={limit} offset={offset}", &input);
        assert_eq!(result, "rust limit=10 offset=5");
    }

    #[test]
    fn normalize_json_array() {
        let value = json!([
            {"title": "t1", "url": "u1", "summary": "s1"},
            {"title": "t2", "url": "u2", "summary": "s2"}
        ]);
        let items = normalize_json_items(&value, "test");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "t1");
        assert_eq!(items[0].source, "test");
    }

    #[test]
    fn normalize_object_with_items_field() {
        let value = json!({
            "items": [{"title": "t1", "url": "u1", "summary": "s1"}]
        });
        let items = normalize_json_items(&value, "test");
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn normalize_uses_link_and_description_fallbacks() {
        let value = json!([
            {"title": "t1", "link": "https://example.com", "description": "desc"}
        ]);
        let items = normalize_json_items(&value, "test");
        assert_eq!(items[0].url, "https://example.com");
        assert_eq!(items[0].summary, "desc");
    }
}
