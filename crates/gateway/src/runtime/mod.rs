#![forbid(unsafe_code)]
#![doc = "Runtime orchestration and control plane for Aman (inlined into gateway)."]

mod agent_runtime;
mod audit;
mod event_store;
mod http;
mod metrics;
mod skill_sync;
mod soul_runtime;
mod tracing_setup;

pub use agent_runtime::{AgentRuntime, AgentRuntimeBuilder, RuntimePhase, RuntimeStatus};
pub use audit::{AuditLogger, AuditRecord};
pub use event_store::EventStore;
pub use http::{serve, HttpServerConfig, HttpServerHandle};
pub use metrics::MetricsRegistry;
pub use soul_runtime::SoulRuntime;
pub use tracing_setup::init_tracing;
