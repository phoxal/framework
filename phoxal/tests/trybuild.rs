//! Compile-pass / compile-fail coverage for the authoring macros.
//!
//! Each `fail/` case pins a compile-time guarantee the surface makes: a
//! capability a role does not have, a handle from the wrong API, a request or
//! response type that does not match the endpoint. The expected diagnostics
//! are part of the guarantee - an author has to be told which rule they hit.

#[test]
fn trybuild_ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/trybuild/pass/*.rs");
    t.compile_fail("tests/trybuild/fail/*.rs");
}
