// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use serde::{Deserialize, Serialize};

/// Resolved configuration for a memory provider backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// Filesystem path to the database file.
    pub db_path: String,
    /// The agent this provider serves.
    pub agent_id: String,
    /// Embedding backend configuration.
    #[serde(default)]
    pub embedding: EmbeddingConfig,
}

/// How to produce embeddings for the memory store.
///
/// Two modes:
/// - **Download**: yantrikdb-managed local ONNX model (e.g. potion-multilingual-128M).
/// - **Remote**: OpenAI-compatible `/v1/embeddings` endpoint. Works with any
///   OpenAI-compatible server (OpenAI, Ollama ≥v0.1.28, oMLX, LM Studio, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode")]
pub enum EmbeddingConfig {
    /// Download a named yantrikdb embedder (e.g. "potion-multilingual-128M").
    #[serde(rename = "download")]
    Download {
        /// Embedder name recognized by yantrikdb's registry.
        name: String,
        /// Expected output dimension.
        dim: usize,
    },
    /// Call a remote OpenAI-compatible `/v1/embeddings` endpoint.
    #[serde(rename = "remote")]
    Remote {
        /// API base URL (e.g. "http://localhost:1234/v1").
        base_url: String,
        /// API key (empty for local servers like Ollama/oMLX/LM Studio).
        api_key: String,
        /// Model name sent in the API request.
        model: String,
        /// Detected output dimension.
        dim: usize,
    },
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self::Download {
            name: "potion-multilingual-128M".to_owned(),
            dim: 256,
        }
    }
}
