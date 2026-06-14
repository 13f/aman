// Copyright (c) 2026 13F
// SPDX-License-Identifier: AGPL-3.0

#[test]
fn ui_macros_compile() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/skill_pass.rs");
    tests.pass("tests/ui/plugin_pass.rs");
    tests.pass("tests/ui/noop_pass.rs");
}
