//! The one end-to-end proof of the compatibility checker: the real registry,
//! the real crates, a real compile.
//!
//! It is ignored by default because it reaches the network and builds the
//! published framework crates from scratch, which belongs in an explicit run
//! rather than in every `cargo test`.

use std::process::Command;

/// The report resolves a real baseline, reads both sides' surfaces, and states
/// every axis it gates on. `report` never fails on the verdict, so this proves
/// the whole path rather than one outcome of it.
#[test]
#[ignore = "reaches the registry and compiles the published crates"]
fn the_published_train_is_a_comparable_baseline() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["compatibility", "report"])
        .output()
        .expect("the runner starts");
    let report = String::from_utf8_lossy(&output.stdout);
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{report}{diagnostics}");
    assert!(report.contains("baseline:"), "{report}");
    assert!(report.contains("workspace:"), "{report}");
    // Read from the same index entries the baseline came from, so a train that
    // resolves at all resolves its floor too.
    assert!(report.contains("toolchain:"), "{report}");
    assert!(
        !report.contains("no comparable baseline"),
        "a surface-carrying train is published, so there is something to compare: {report}"
    );
    assert!(report.contains("impact:"), "{report}");
    assert!(report.contains("release:"), "{report}");
}

/// The Stable-line drill runs end to end and holds, which is what makes it
/// usable as a scheduled job and as the first half of the flip-day gate.
///
/// Ignored for the same reason as the comparison above: it drives `cargo test`
/// subprocesses. The matrix itself is proved without them, in the drill's own
/// unit tests, and the scheduled CI job is where the whole command runs.
#[test]
#[ignore = "runs the pinned tests as cargo subprocesses"]
fn the_stable_line_rehearsal_holds() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["compatibility", "rehearse-v1"])
        .output()
        .expect("the runner starts");
    let report = String::from_utf8_lossy(&output.stdout);
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{report}{diagnostics}");
    assert!(!report.contains("FAIL"), "{report}");
    assert!(report.contains("drilled"), "{report}");
}
