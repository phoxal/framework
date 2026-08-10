//! The one end-to-end proof of the compatibility checker: the real registry,
//! the real crates, a real compile.
//!
//! It is ignored by default because it reaches the network and builds the
//! published framework crates from scratch, which belongs in an explicit run
//! rather than in every `cargo test`.

use std::process::Command;

/// Until a train carrying contract surfaces is published, the newest published
/// crates have no surface to state, and the checker says so and passes rather
/// than reporting a vacuously unchanged contract.
#[test]
#[ignore = "reaches the registry and compiles the published crates"]
fn the_published_train_has_no_comparable_baseline() {
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["compatibility", "report"])
        .output()
        .expect("the runner starts");
    let report = String::from_utf8_lossy(&output.stdout);
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{report}{diagnostics}");
    assert!(report.contains("baseline:"), "{report}");
    assert!(report.contains("no comparable baseline"), "{report}");
}
