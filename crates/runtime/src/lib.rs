#![forbid(unsafe_code)]
#![doc = "Runtime orchestration and control plane for Aman."]

mod agent_runtime;
mod audit;
mod http;
mod event_store;
mod metrics;
mod soul_runtime;
mod tracing_setup;

pub use agent_runtime::{AgentRuntime, AgentRuntimeBuilder, RuntimePhase, RuntimeStatus};
pub use audit::{AuditLogger, AuditRecord};
pub use event_store::EventStore;
pub use http::{serve, HttpServerConfig, HttpServerHandle};
pub use soul_runtime::SoulRuntime;
pub use tracing_setup::init_tracing;
