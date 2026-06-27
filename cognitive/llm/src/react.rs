// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Re-export of ReAct loop types from `cognitive-react`.
//!
//! These types were previously defined here directly. They now live in
//! the `cognitive-react` leaf crate, which has zero dependencies on
//! `kernel` and can be shared by both the cognitive engine and the
//! gateway without creating a dependency cycle.
//!
//! This module is kept as a re-export for backward compatibility within
//! `cognitive-llm`. New code should import from `cognitive_react` directly.

pub use cognitive_react::*;
