//! Compile-fail coverage for the Runtime and Scenario macros.
//!
//! Each `fail/` case pins one compile-time guarantee the surface makes: a
//! capability a role does not have, a handle from the wrong API, a request
//! or response type that does not match the endpoint, or a scenario
//! function with the wrong shape. The expected diagnostics are part of
//! the guarantee - an author has to be told which rule they hit.
//! Successful authored usage is covered deterministically by
//! `runtime_authoring` and `runtime_semantics`, whose 106 tests compile
//! and run the real macro surfaces without invoking the compiler again.

#[test]
fn trybuild_ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile/fail/*.rs");
}

// Scenario authoring surface: positive and negative compile coverage for
// the function-to-`#[test]` attribute and its exact mutable fixture
// signature.
#[cfg(feature = "scenario")]
#[test]
fn trybuild_scenario_ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/compile/scenario_pass/*.rs");
    t.compile_fail("tests/compile/scenario_fail/*.rs");
}
