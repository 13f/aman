#![forbid(unsafe_code)]
#![doc = "Persistence layer primitives for the aman agent framework."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


mod deferred_file;
mod dlq;
mod index_store;
mod overflow;
mod persistent_bus;
mod state_store;
mod trace_store;
mod wal;

pub use deferred_file::FileDeferredTaskQueue;
pub use dlq::{
    DeadLetterEntry, DeadLetterQueue, DlqExpiryAlert, DlqFilter, DlqRetryRecord,
    InMemoryDeadLetterQueue,
};
pub use index_store::{IndexRecord, IndexStore};
pub use overflow::OverflowDir;
pub use persistent_bus::{PersistentBus, PersistentBusConfig};
pub use state_store::{
    CleanupPolicy, IsolationMode, SledStore, StateRecord, StateStore, WriteConsistency,
};
pub use trace_store::JsonlTraceStore;
pub use wal::{WalSync, WriteAheadLog};
