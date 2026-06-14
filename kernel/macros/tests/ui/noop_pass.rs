// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

//! trybuild pass-test for the `#[derive(Noop)]` proc-macro.
//!
//! Verifies that the derive expands without errors. The trait path
//! (`crate::PluginExportRegistrar`) is resolved relative to the
//! `plugin` crate that defines the trait, so this test must live
//! next to that crate (or in a crate that re-exports it under
//! `crate::`).

use macros::Noop;
use plugin::PluginExportRegistrar;

#[allow(dead_code)]
#[derive(Default, Noop)]
pub struct TestNoopRegistrar;

#[allow(dead_code)]
fn _assert_impl_plugin_export_registrar(r: TestNoopRegistrar) {
    // Compile-time proof that the generated impl resolves.
    let _: Box<dyn PluginExportRegistrar> = Box::new(r);
}

fn main() {}
