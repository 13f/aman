// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Initialize the global tracing subscriber.
///
/// Sets up a subscriber that writes structured logs to stderr at INFO
/// level by default. The log level can be overridden via the
/// `AMAN_LOG` environment variable (e.g. `AMAN_LOG=debug`).
///
/// Safe to call multiple times — only the first call has an effect.
pub fn init_tracing() {
    let filter = EnvFilter::try_from_env("AMAN_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_line_number(true);

    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer);

    let _ = subscriber.try_init();
}
