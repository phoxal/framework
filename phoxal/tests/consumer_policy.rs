//! Compilation checks for the public consumer feature profiles.
//!
//! Each supported consumer configuration must compile exactly as
//! published. These checks prove the configurations work; they do not
//! assert an exact dependency graph — the profiles' dependency roles
//! (transport-free base, client-only session, runner-free scenario) are
//! a design property of the manifest, not something to re-derive from
//! `cargo tree` output here.

use std::path::Path;
use std::process::Command;

fn check_profile(features: &str, default_features: bool) {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let manifest = manifest
        .to_str()
        .unwrap_or_else(|| panic!("the manifest path is UTF-8"));
    let profile = if features.is_empty() {
        "default"
    } else {
        features
    };
    let mut command = Command::new(cargo);
    command.args(["check", "--manifest-path", manifest, "--offline"]);
    if !default_features {
        command.arg("--no-default-features");
    }
    if !features.is_empty() {
        command.args(["--features", features]);
    }
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("cargo check for profile `{profile}` starts: {error}"));
    assert!(
        output.status.success(),
        "the `{profile}` consumer profile no longer compiles:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_base_sdk_profile_compiles() {
    check_profile("", false);
}

#[test]
fn the_runtime_profile_compiles() {
    check_profile("runtime", false);
}

#[test]
fn the_session_profile_compiles() {
    check_profile("session", false);
}

#[test]
fn the_scenario_profile_compiles() {
    check_profile("scenario", false);
}

#[test]
fn the_build_profile_compiles() {
    check_profile("build", false);
}

#[test]
fn the_default_profile_compiles() {
    check_profile("", true);
}
