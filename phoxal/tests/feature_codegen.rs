//! Isolated verification that the SDK's `robotics` feature gates the
//! build-time generation of `phoxal-robotics-descriptors.bin`.
//!
//! The `phoxal::robotics` module is a real gated SDK module: the
//! `pub mod robotics;` declaration is hidden unless Cargo enables the
//! `robotics` feature, and the build script only runs the robotics
//! compilation when `CARGO_FEATURE_ROBOTICS` is set. This test
//! builds the SDK into a fresh `target` directory under each feature
//! flag and inspects the generated `OUT_DIR` to confirm the feature
//! boundary is real at build time, not only at the Rust source level.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cargo() -> std::ffi::OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

fn build_phoxal(target_dir: &Path, features: &[&str]) {
    let mut command = Command::new(cargo());
    command
        .args(["build", "-p", "phoxal", "--no-default-features"])
        .args(features.iter().flat_map(|f| ["--features", f]))
        .args(["--target-dir"])
        .arg(target_dir);
    let status = command
        .status()
        .expect("cargo build for phoxal feature-codegen test starts");
    assert!(
        status.success(),
        "cargo build {:?} must succeed for feature-codegen test",
        features
    );
}

/// Walks `target_dir/debug/build/phoxal-*/out/` and returns every file whose
/// name matches the given file name, one path per match.
fn files_under_phoxal_out(target_dir: &Path, file_name: &str) -> Vec<PathBuf> {
    let build_dir = target_dir.join("debug").join("build");
    let Ok(entries) = std::fs::read_dir(&build_dir) else {
        return Vec::new();
    };
    let mut matches = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("phoxal-") {
            continue;
        }
        let candidate = entry.path().join("out").join(file_name);
        if candidate.exists() {
            matches.push(candidate);
        }
    }
    matches
}

#[test]
fn port_feature_does_not_generate_robotics_descriptor() {
    let target = tempfile::tempdir().expect("temp target dir for port build");
    build_phoxal(target.path(), &["port"]);
    let descriptor =
        files_under_phoxal_out(target.path(), "phoxal-robotics-descriptors.bin");
    assert!(
        descriptor.is_empty(),
        "port feature must not generate the robotics descriptor; found {descriptor:?}"
    );
}

#[test]
fn robotics_feature_generates_robotics_descriptor() {
    let target = tempfile::tempdir().expect("temp target dir for robotics build");
    build_phoxal(target.path(), &["robotics"]);
    let descriptor =
        files_under_phoxal_out(target.path(), "phoxal-robotics-descriptors.bin");
    assert_eq!(
        descriptor.len(),
        1,
        "robotics feature must generate exactly one robotics descriptor; found {descriptor:?}"
    );
}