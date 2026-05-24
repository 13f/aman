use async_trait::async_trait;
use kernel::AmanResult;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, warn};

use super::Adapter;
use crate::types::{InfoItem, InfoSearchInput};

pub struct EmbeddingAdapter {
    source_name: String,
    base_url: String,
    model: String,
    api_key: Option<String>,
    db_path: String,
    threshold: f64,
    max_candidates: usize,
    timeout_ms: u64,
}

impl EmbeddingAdapter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_name: String,
        base_url: String,
        model: String,
        api_key: Option<String>,
        db_path: String,
        threshold: f64,
        max_candidates: usize,
        timeout_ms: u64,
    ) -> Self {
        Self {
            source_name,
            base_url,
            model,
            api_key,
            db_path: super::expand_tilde(&db_path),
            threshold,
            max_candidates,
            timeout_ms,
        }
    }
}

#[async_trait]
impl Adapter for EmbeddingAdapter {
    async fn search(&self, input: &InfoSearchInput) -> AmanResult<Vec<InfoItem>> {
        // 1. Fetch candidate articles from the backing SQLite database
        let candidates = self.fetch_candidates().await?;
        if candidates.is_empty() {
            debug!(source = %self.source_name, "EmbeddingAdapter: no candidates in DB");
            return Ok(Vec::new());
        }

        // 2. Build texts to embed: query + candidate title|summary pairs
        let candidate_texts: Vec<String> = candidates
            .iter()
            .map(|c| format!("{} | {}", c.title, c.summary))
            .collect();

        let mut all_texts = vec![input.query.clone()];
        all_texts.extend(candidate_texts.clone());

        // 3. Generate embeddings for query + all candidates in one batch
        let embeddings = self.embed_batch(&all_texts).await?;
        if embeddings.len() != all_texts.len() {
            warn!(source = %self.source_name, expected = all_texts.len(), got = embeddings.len(),
                  "EmbeddingAdapter: embedding count mismatch");
            return Ok(Vec::new());
        }

        let query_embedding = &embeddings[0];
        let candidate_embeddings = &embeddings[1..];

        // 4. Compute cosine similarity and filter by threshold
        let mut scored: Vec<(f64, &InfoItem)> = candidates
            .iter()
            .zip(candidate_embeddings.iter())
            .map(|(item, emb)| (cosine_similarity(query_embedding, emb), item))
            .filter(|(score, _)| *score >= self.threshold)
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        debug!(
            source = %self.source_name,
            candidates = candidates.len(),
            matched = scored.len(),
            threshold = self.threshold,
            "EmbeddingAdapter: semantic search complete",
        );

        Ok(scored.into_iter().map(|(_, item)| item.clone()).collect())
    }
}

impl EmbeddingAdapter {
    /// Fetch candidate articles from the SQLite database.
    ///
    /// Supports two schemas:
    /// - Fusion RSS: `items` JOIN `feeds`
    /// - Standalone: `articles` table
    async fn fetch_candidates(&self) -> AmanResult<Vec<InfoItem>> {
        let conn = rusqlite::Connection::open(&self.db_path).map_err(|e| {
            kernel::Error::config_invalid(format!("open embedding db {}: {e}", self.db_path))
        })?;

        // Detect schema: fusion-style has `items` + `feeds` tables
        let is_fusion: bool = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='items'")
            .and_then(|mut s| s.exists([]))
            .unwrap_or(false);

        let mut items = if is_fusion {
            self.fetch_fusion_candidates(&conn)?
        } else {
            self.fetch_standalone_candidates(&conn)?
        };

        // Set source name on all items
        for item in &mut items {
            item.source = self.source_name.clone();
        }

        Ok(items)
    }

    fn fetch_fusion_candidates(
        &self,
        conn: &rusqlite::Connection,
    ) -> AmanResult<Vec<InfoItem>> {
        let mut stmt = conn
            .prepare(
                "SELECT i.title, i.link, i.content, i.pub_date, f.name as feed_name
                 FROM items i
                 JOIN feeds f ON i.feed_id = f.id
                 ORDER BY i.pub_date DESC
                 LIMIT ?",
            )
            .map_err(|e| kernel::Error::config_invalid(format!("fusion query: {e}")))?;

        let rows = stmt
            .query_map([self.max_candidates as i64], |row| {
                let title: String = row.get(0)?;
                let link: String = row.get(1)?;
                let content: Option<String> = row.get(2)?;
                let pub_date: Option<i64> = row.get(3)?;
                let feed_name: String = row.get(4)?;
                Ok((title, link, content, pub_date, feed_name))
            })
            .map_err(|e| kernel::Error::config_invalid(format!("fusion query map: {e}")))?;

        let mut items = Vec::new();
        for row in rows {
            let (title, link, content, pub_date, _feed_name) = row.map_err(|e| {
                kernel::Error::config_invalid(format!("fusion row: {e}"))
            })?;
            let published = pub_date.and_then(|ts| {
                if ts > 0 {
                    Some(ts.to_string())
                } else {
                    None
                }
            });
            items.push(InfoItem {
                title,
                url: link,
                summary: content.unwrap_or_default(),
                published,
                source: String::new(),
                raw: serde_json::json!({}),
            });
        }
        Ok(items)
    }

    fn fetch_standalone_candidates(
        &self,
        conn: &rusqlite::Connection,
    ) -> AmanResult<Vec<InfoItem>> {
        let mut stmt = conn
            .prepare(
                "SELECT title, link, description, pub_date, source_name
                 FROM articles
                 ORDER BY pub_date DESC
                 LIMIT ?",
            )
            .map_err(|e| kernel::Error::config_invalid(format!("articles query: {e}")))?;

        let rows = stmt
            .query_map([self.max_candidates as i64], |row| {
                let title: String = row.get(0)?;
                let link: String = row.get(1)?;
                let description: Option<String> = row.get(2)?;
                let pub_date: Option<String> = row.get(3)?;
                let source_name: String = row.get(4)?;
                Ok((title, link, description, pub_date, source_name))
            })
            .map_err(|e| kernel::Error::config_invalid(format!("articles query map: {e}")))?;

        let mut items = Vec::new();
        for row in rows {
            let (title, link, description, pub_date, _src) = row.map_err(|e| {
                kernel::Error::config_invalid(format!("articles row: {e}"))
            })?;
            items.push(InfoItem {
                title,
                url: link,
                summary: description.unwrap_or_default(),
                published: pub_date,
                source: String::new(),
                raw: serde_json::json!({}),
            });
        }
        Ok(items)
    }

    /// Call the OpenAI-compatible embeddings endpoint for a batch of texts.
    async fn embed_batch(&self, texts: &[String]) -> AmanResult<Vec<Vec<f64>>> {
        #[derive(Serialize)]
        struct EmbedRequest<'a> {
            model: &'a str,
            input: &'a [String],
        }

        #[derive(Deserialize)]
        struct EmbedResponse {
            data: Vec<EmbedData>,
        }

        #[derive(Deserialize)]
        struct EmbedData {
            embedding: Vec<f64>,
        }

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(self.timeout_ms))
            .build()
            .map_err(|e| kernel::Error::config_invalid(format!("embedding client: {e}")))?;

        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let body = EmbedRequest {
            model: &self.model,
            input: texts,
        };

        let mut req = client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body);

        if let Some(key) = &self.api_key {
            req = req.header("Authorization", &format!("Bearer {key}"));
        }

        let resp = req.send().await.map_err(|e| {
            kernel::Error::config_invalid(format!("embedding request {url}: {e}"))
        })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(kernel::Error::config_invalid(format!(
                "embedding API {url}: HTTP {status} — {body}"
            )));
        }

        let data: EmbedResponse = resp.json().await.map_err(|e| {
            kernel::Error::config_invalid(format!("embedding response parse: {e}"))
        })?;

        Ok(data.data.into_iter().map(|d| d.embedding).collect())
    }
}

/// Compute cosine similarity between two vectors.
fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_vectors() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 0.001);
    }

    #[test]
    fn cosine_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.0).abs() < 0.001);
    }

    #[test]
    fn cosine_opposite_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim + 1.0).abs() < 0.001);
    }

    #[test]
    fn cosine_empty_returns_zero() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn cosine_mismatched_lengths_returns_zero() {
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
    }
}
