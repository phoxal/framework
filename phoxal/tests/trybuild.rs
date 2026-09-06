//! Compile-pass / compile-fail coverage for the authoring macros.
//!
//! Each `fail/` case pins a compile-time guarantee the surface makes: a
//! capability a role does not have, a handle from the wrong API, a request or
//! response type that does not match the endpoint. The expected diagnostics
//! are part of the guarantee - an author has to be told which rule they hit.

#[cfg(not(feature = "test-harness"))]
#[test]
fn trybuild_ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/trybuild/pass/*.rs");
    t.compile_fail("tests/trybuild/fail/*.rs");
}

// Workspace-wide test builds may unify the explicit dev-dependency feature
// from an official component into this package's test target. The negative
// surface test is meaningful only for the production feature set; with the
// harness deliberately enabled, compiling its import is expected.
#[cfg(feature = "test-harness")]
#[test]
fn trybuild_ui_with_test_harness_enabled() {}

// The cases where a *host* profile is the subject. A participant cannot name
// host families at all; these cases pin type separation for consumers that can
// name those families.
#[cfg(feature = "session")]
#[test]
fn trybuild_host_ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/trybuild/host_pass/*.rs");
    t.compile_fail("tests/trybuild/host_fail/*.rs");
}
