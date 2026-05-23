#![forbid(unsafe_code)]
#![doc = "aman Agent Framework SDK.

This crate provides a single-dependency entry point for external Skill and Plugin
developers. Re-exports the most important public types from across the framework.

## Example

```rust
use sdk::prelude::*;

// Core types: Event, EventType, Skill, Tool, Pipeline
// Runtime: AgentRuntime, AgentRuntimeBuilder, serve
// Config: AgentConfig
// Source: SourceRegistry
// Workflow: WorkflowDef, WorkflowEngine, StateDef, Transition
// SOUL: Soul
// Plugin: PluginManifest
```
"]

/// Core types and traits (Event, EventType, Skill, Tool, Pipeline, etc.)
pub use kernel;

/// Agent configuration
pub use config;

/// Event bus types
pub use event_bus;

/// Persistence (WAL, StateStore, DeadLetterQueue)
pub use persistence;

/// Plugin lifecycle
pub use plugin;

/// Agent runtime (re-exported from gateway crate)
pub use gateway::runtime;

/// Secret management
pub use secret;

/// Skill registry and management
pub use skill;

/// SOUL system prompt
pub use soul;

/// Event sources
pub use source;

/// Tool registry
pub use tool;

/// Workflow engine
pub use workflow;

pub mod prelude {
    //! Convenience re-exports for aman Skill and Plugin authors.

    pub use kernel::prelude::*;

    pub use gateway::runtime::{
        serve, AgentRuntime, AgentRuntimeBuilder, HttpServerConfig, HttpServerHandle, RuntimePhase,
        RuntimeStatus,
    };

    pub use config::AgentConfig;
    pub use source::SourceRegistry;

    pub use workflow::{
        ErrorRecovery, RetryFailurePolicy, StateDef, StateTimeout, Transition, TransitionAction,
        TransitionFrom, TransitionTo, WorkflowDef, WorkflowEngine,
    };

    pub use soul::Soul;
    pub use plugin::{PluginLifecycleState, PluginManifest};
}
