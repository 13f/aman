#![forbid(unsafe_code)]
#![doc = "Persistence layer primitives for the Aman agent framework."]

mod dlq;
mod overflow;
mod persistent_bus;
mod state_store;
mod wal;

pub use dlq::{
    DeadLetterEntry, DeadLetterQueue, DlqExpiryAlert, DlqFilter, DlqRetryRecord,
    InMemoryDeadLetterQueue,
};
pub use overflow::OverflowDir;
pub use persistent_bus::{PersistentBus, PersistentBusConfig};
pub use state_store::{
    CleanupPolicy, IsolationMode, SledStore, StateRecord, StateStore, WriteConsistency,
};
pub use wal::{WalSync, WriteAheadLog};
