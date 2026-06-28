//! Compile-pass / compile-fail coverage for the authoring macros (D60, step 16).

#[test]
fn trybuild_ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/trybuild/pass/*.rs");
    t.compile_fail("tests/trybuild/fail/*.rs");
}
