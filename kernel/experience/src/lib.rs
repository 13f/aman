// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! Experience system — tool strategies, patterns, and anti-patterns stored in EXP.md.
//!
//! EXP.md is the agent's muscle memory: unlike memory (episodic/semantic knowledge
//! with 30-day half-life), experience captures *how to do things* — which tool
//! combinations work, which patterns to avoid, which gotchas have been hit.
//!
//! ## Three layers of knowledge
//!
//! | Layer | Storage | Decay | Purpose |
//! |---|---|---|---|
//! | Identity | SOUL.md | None | Who I am |
//! | **Experience** | **EXP.md** | **No decay** (entries can be marked "needs_verification") | **How I do things** |
//! | Knowledge | yantrikdb | 30-day half-life | What I know |

#![forbid(unsafe_code)]

pub mod exp_md;
pub mod model;

pub use model::{ExperienceEntry, ExperienceKind, ExperienceTag, ExpMd};
