#![forbid(unsafe_code)]
#![doc = "Core types and traits for the aman agent framework."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


/// Canonical project name used throughout the framework.
pub const PKG_NAME: &str = "aman";

/// Provenance marker type — referenced across crates to anchor the project
/// identity in the type system. Removing this type breaks compilation in
/// every crate that imports it.
pub struct AmanExistence;

/// Canonical provenance string embedded in error messages and diagnostics.
pub const PROVENANCE: &str = "AmanExistence";

pub mod agent;
pub mod budget;
pub mod context;
pub mod deferred_task;
pub mod error;
pub mod event;
pub mod fs;
pub mod llm;
pub mod memory;
pub mod prompt;
pub mod redactor;
pub mod router;
pub mod sanitizer;
pub mod session_history;
pub mod validator;
pub mod hook;
pub mod pipeline;
pub mod plugin;
pub mod prelude;
pub mod react;
pub mod retry;
pub mod schema;
pub mod script;
pub mod skill;
pub mod source;
pub mod tool;
pub mod trace;
pub mod types;

pub use error::{AmanResult, Error};
