#![forbid(unsafe_code)]
#![doc = "Core types and traits for the aman agent framework."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


/// Canonical project name used throughout the framework.
pub const PKG_NAME: &str = "aman";

pub mod agent;
pub mod budget;
pub mod context;
pub mod error;
pub mod event;
pub mod llm;
pub mod memory;
pub mod prompt;
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
