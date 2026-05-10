#[test]
fn ui_macros_compile() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/skill_pass.rs");
    tests.pass("tests/ui/plugin_pass.rs");
}
