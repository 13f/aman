#![forbid(unsafe_code)]
#![doc = "Runtime orchestration and control plane for aman (inlined into gateway)."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


mod agent_harness;
mod agent_registry;
mod agent_runtime;
mod date_util;
mod history_compressor;
mod memory_store;
mod token_budget;
mod audit;
mod event_store;
mod http;
mod metrics;
mod session_store;
mod skill_sync;
mod agent_seed;
mod soul_runtime;
mod tracing_setup;

pub use agent_registry::AgentRegistry;
pub use date_util::current_date_string;
pub use agent_runtime::{AgentRuntime, AgentRuntimeBuilder, RuntimePhase, RuntimeStatus};
pub use audit::{AuditLogger, AuditRecord};
pub use event_store::EventStore;
pub use http::{serve, HttpServerConfig, HttpServerHandle};
pub use metrics::MetricsRegistry;
pub use kernel::memory::MemoryEntry;
pub use memory_store::MemoryStore;
pub use session_store::{SessionRecord, SessionStore};
pub use soul_runtime::SoulRuntime;
pub use tracing_setup::init_tracing;

/// Re-export the notification store type for use in HTTP handlers and Tauri bridge.
pub use notification::NotificationStore;
