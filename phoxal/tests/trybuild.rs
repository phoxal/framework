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

// The cases a *host* profile is the subject of. A participant cannot name the
// runtime family at all, which `fail/participant_cannot_reach_the_world_clock`
// pins; the guarantee below is the one that still has to hold for a consumer
// that *can* name it - the world clock is a sibling semantic of `State`, never
// a subtype of it, so no ordinary state publisher can mint a world step.
#[cfg(feature = "session")]
#[test]
fn trybuild_host_ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/trybuild/host_pass/*.rs");
    t.compile_fail("tests/trybuild/host_fail/*.rs");
}
