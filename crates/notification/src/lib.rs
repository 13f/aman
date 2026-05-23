#![forbid(unsafe_code)]
#![doc = "Notification center — severity-classed user-facing alerts for the aman agent runtime."]
// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0


pub mod model;
pub mod store;
pub mod subscriber;

pub use model::{Category, Notification, Severity};
pub use store::NotificationStore;
pub use subscriber::NotificationSubscriber;
