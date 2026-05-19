#![forbid(unsafe_code)]
#![doc = "Runtime orchestration and control plane for Aman (inlined into gateway)."]

mod agent_harness;
mod agent_registry;
mod agent_runtime;
mod history_compressor;
mod token_budget;
mod audit;
mod event_store;
mod http;
mod metrics;
mod session_store;
mod skill_sync;
mod soul_runtime;
mod tracing_setup;

pub use agent_registry::AgentRegistry;
pub use agent_runtime::{AgentRuntime, AgentRuntimeBuilder, RuntimePhase, RuntimeStatus};
pub use audit::{AuditLogger, AuditRecord};
pub use event_store::EventStore;
pub use http::{serve, HttpServerConfig, HttpServerHandle};
pub use metrics::MetricsRegistry;
pub use session_store::{SessionRecord, SessionStore};
pub use soul_runtime::SoulRuntime;
pub use tracing_setup::init_tracing;

/// Re-export the notification store type for use in HTTP handlers and Tauri bridge.
pub use notification::NotificationStore;
