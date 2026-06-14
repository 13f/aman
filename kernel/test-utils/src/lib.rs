#![forbid(unsafe_code)]

//! Shared test utilities for the Aman workspace.
//!
//! ## Quick start
//!
//! The most common pattern is publishing events through a fake bus and
//! asserting on what was recorded:
//!
//! ```no_run
//! use std::sync::Arc;
//! use event_bus::EventBus;
//! use test_utils::fake_event_bus::{FakeBusConfig, FakeEventBus};
//! use kernel::event::{Event, EventType};
//! use serde_json::json;
//!
//! # async fn example() {
//! let bus: Arc<FakeEventBus> = Arc::new(FakeEventBus::new(FakeBusConfig::default()));
//! bus.publish(Event::new(
//!     "test",
//!     EventType::Custom("example.event".into()),
//!     json!({ "k": 1 }),
//! ))
//! .await
//! .unwrap();
//!
//! assert_eq!(bus.event_count(), 1);
//! assert!(bus.has_event(|e| matches!(&e.event_type,
//!     EventType::Custom(t) if t == "example.event")));
//! # }
//! ```
//!
//! For LLM providers, see [`mock_llm::MockLLMProvider`] — it implements
//! the kernel-style `LlmProvider` trait and supports per-call configs,
//! delays, and `error_on_nth_call` for chaos testing. For wall-clock
//! control, use [`clock::DeterministicClock`] in place of
//! `SystemTime::now()`; it is **passive** — production code must call
//! `clock.now()` to benefit.

pub mod clock;
pub mod fake_event_bus;
pub mod mock_llm;
