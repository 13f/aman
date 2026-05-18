#![forbid(unsafe_code)]
#![doc = "Notification center — severity-classed user-facing alerts for the Aman agent runtime."]

pub mod model;
pub mod store;
pub mod subscriber;

pub use model::{Category, Notification, Severity};
pub use store::NotificationStore;
pub use subscriber::NotificationSubscriber;
