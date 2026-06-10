#[test]
fn topic_tree_compile_failures() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/ui/topic_drive_has_no_motor.rs");
    tests.compile_fail("tests/ui/topic_wrong_payload.rs");
}
