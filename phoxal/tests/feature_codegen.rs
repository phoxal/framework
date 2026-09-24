//! Isolated verification of feature-scoped SDK robotics generation.
//!
//! The SDK compiles its robotics vocabulary from owned Protobuf sources only
//! when the robotics feature is selected.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cargo() -> std::ffi::OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

fn build_phoxal(target_dir: &Path, features: &[&str]) {
    let mut command = Command::new(cargo());
    command
        .args([
            "build",
            "-p",
            "phoxal",
            "--offline",
            "--no-default-features",
        ])
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
    let descriptor = files_under_phoxal_out(target.path(), "robotics-descriptors.bin");
    assert!(
        descriptor.is_empty(),
        "contract feature must not generate the robotics descriptor; found {descriptor:?}"
    );
}

#[test]
fn robotics_feature_generates_one_local_descriptor() {
    let target = tempfile::tempdir().expect("temp target dir for robotics build");
    build_phoxal(target.path(), &["robotics"]);
    let descriptor = files_under_phoxal_out(target.path(), "robotics-descriptors.bin");
    assert!(
        descriptor.len() == 1,
        "robotics feature must generate one local descriptor; found {descriptor:?}"
    );
}
