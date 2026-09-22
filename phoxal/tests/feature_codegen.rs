//! Isolated verification that SDK features do not revive build-time robotics
//! code generation.
//!
//! The `phoxal::robotics` module remains feature-gated, but its generated Rust
//! and descriptor closure are checked-in package inputs. The build script owns
//! only runtime protocols. These tests build the SDK into a fresh target under
//! the contract-only and robotics profiles and ensure neither profile creates
//! a second robotics descriptor in `OUT_DIR`.

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
    let status = command.status().unwrap_or_else(|error| {
        panic!("cargo build for phoxal feature-codegen test did not start: {error}")
    });
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
fn contract_feature_does_not_generate_robotics_descriptor() {
    let target = tempfile::tempdir().expect("temp target dir for contract build");
    build_phoxal(target.path(), &["contract"]);
    let descriptor = files_under_phoxal_out(target.path(), "phoxal-robotics-descriptors.bin");
    assert!(
        descriptor.is_empty(),
        "contract feature must not generate the robotics descriptor; found {descriptor:?}"
    );
}

#[test]
fn robotics_feature_uses_the_checked_in_descriptor() {
    let target = tempfile::tempdir().expect("temp target dir for robotics build");
    build_phoxal(target.path(), &["robotics"]);
    let descriptor = files_under_phoxal_out(target.path(), "phoxal-robotics-descriptors.bin");
    assert!(
        descriptor.is_empty(),
        "robotics feature must not generate a second descriptor; found {descriptor:?}"
    );
    assert!(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("generated/robotics/phoxal-descriptors.bin")
            .is_file(),
        "robotics package must contain its checked-in descriptor closure"
    );
}
