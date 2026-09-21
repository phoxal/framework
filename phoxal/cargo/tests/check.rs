//! Integration tests for `cargo phoxal check`.
//!
//! Each test stages a committed fixture under `tests/fixtures/` into a
//! fresh tempdir, spawns the compiled `cargo-phoxal` binary, and asserts
//! on its exit code and captured output. The integration tests treat the
//! binary as an opaque subprocess the same way a downstream user would;
//! they do not import anything from `cargo_phoxal` or `phoxal`.

mod support;

use std::fs;

use support::stage;

#[test]
fn check_passes_a_valid_robot_and_writes_a_lockfile() {
    let (_guard, root) = stage("check-valid");
    let populate = support::invoke(&root, &["check", "--offline"]);
    assert!(
        populate.status.success(),
        "first check must populate the lockfile:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&populate.stdout),
        String::from_utf8_lossy(&populate.stderr),
    );
    assert!(
        fs::read_to_string(root.join("Cargo.lock"))
            .unwrap_or_else(|error| panic!("read generated Cargo.lock: {error}"))
            .contains("[[package]]"),
        "first check must emit a Cargo.lock at the project root"
    );

    let locked_run = support::invoke(&root, &["check", "--locked", "--offline"]);
    assert!(
        locked_run.status.success(),
        "second check must honour --locked against the populated lockfile:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&locked_run.stdout),
        String::from_utf8_lossy(&locked_run.stderr),
    );
}

#[test]
fn check_reports_a_missing_runtime_artifact_contract() {
    let (_guard, root) = stage("check-missing-artifact");
    let output = support::invoke(&root, &["check", "--offline"]);
    assert!(
        !output.status.success(),
        "missing artifact must yield a non-zero exit, got {}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Phoxal Runtime contract"),
        "stderr must mention the missing contract, got:\n{stderr}"
    );
}

#[test]
fn check_rejects_an_undeclared_service_implementation() {
    let (_guard, root) = stage("check-bad-deps");
    let output = support::invoke(&root, &["check", "--offline"]);
    assert!(
        !output.status.success(),
        "undeclared service must yield a non-zero exit, got {}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does-not-exist"),
        "stderr must name the bad dependency key, got:\n{stderr}"
    );
    assert!(
        stderr.contains("not a normal direct dependency"),
        "stderr must surface the source-selection refusal, got:\n{stderr}"
    );
}

#[test]
fn check_refuses_locked_mode_when_initialization_is_missing() {
    let (_guard, root) = stage("check-locked-no-init");
    let output = support::invoke(&root, &["check", "--locked", "--offline"]);
    assert!(
        !output.status.success(),
        "locked mode with no Cargo.lock must yield a non-zero exit, got {}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("phoxal-supervisor") && stderr.contains("--locked"),
        "stderr must identify the missing dependency and the locked mode, got:\n{stderr}"
    );
}
