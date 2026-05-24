use async_trait::async_trait;
use kernel::AmanResult;
use std::collections::HashMap;
use tracing::debug;

use super::{normalize_json_items, replace_placeholders, resolve_env, Adapter};
use crate::types::{InfoItem, InfoSearchInput};

pub struct ApiAdapter {
    source_name: String,
    url_template: String,
    api_key: Option<String>,
    headers: HashMap<String, String>,
    timeout_ms: u64,
}

impl ApiAdapter {
    pub fn new(
        source_name: String,
        url_template: String,
        api_key: Option<String>,
        headers: HashMap<String, String>,
        timeout_ms: u64,
    ) -> Self {
        Self {
            source_name,
            url_template,
            api_key,
            headers,
            timeout_ms,
        }
    }
}

#[async_trait]
impl Adapter for ApiAdapter {
    async fn search(&self, input: &InfoSearchInput) -> AmanResult<Vec<InfoItem>> {
        let url = replace_placeholders(&self.url_template, input);
        debug!(source = %self.source_name, %url, "info-hub api request");

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(self.timeout_ms))
            .build()
            .map_err(|e| kernel::Error::config_invalid(format!("reqwest client build: {e}")))?;

        let mut req = client.get(&url);

        if let Some(key) = &self.api_key {
            let resolved = resolve_env(key);
            if !resolved.is_empty() {
                req = req.header("Authorization", &resolved);
            }
        }

        for (name, value) in &self.headers {
            req = req.header(name.as_str(), resolve_env(value));
        }

        req = req.header("Accept", "application/json");

        let resp = req.send().await.map_err(|e| {
            kernel::Error::config_invalid(format!("api request failed for {url}: {e}"))
        })?;

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            kernel::Error::config_invalid(format!("api response parse for {url}: {e}"))
        })?;

        Ok(normalize_json_items(&body, &self.source_name))
    }
}
