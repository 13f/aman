#![forbid(unsafe_code)]
#![doc = "Idle state system for the aman agent framework."]

pub mod arousal;
pub mod config;
pub mod coordination;
pub mod detector;
pub mod incubation;
pub mod manager;
pub mod metrics;
pub mod personality;
pub mod types;
pub mod workflow;

pub use types::{
    ChatMode, IdleContext, IdleEvent, IdleKind, IdlePersonality, QueueDrained, ArousalBehavior,
};
pub use coordination::IdleCoordination;
pub use manager::AgentIdleManager;
