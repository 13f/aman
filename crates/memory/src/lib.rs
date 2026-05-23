// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Memory providers for the aman agent framework.
//!
//! Each provider implements [`kernel::memory::MemoryProvider`].
//! Currently the default (and only) backend is [`YantrikdbProvider`].

#![forbid(unsafe_code)]

pub mod config;
pub mod remote_embedder;
pub mod yantrikdb;

pub use config::{EmbeddingConfig, MemoryConfig};
pub use remote_embedder::RemoteEmbedder;
pub use yantrikdb::YantrikdbProvider;
