// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Re-export of ReAct loop types from `cognitive-react`.
//!
//! These types were previously defined here as a deprecated shim. They now
//! live in the `cognitive-react` leaf crate. This module is kept for
//! backward compatibility — all existing imports of `kernel::react::*`
//! continue to work.

pub use cognitive_react::*;
