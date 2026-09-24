//! Cargo-level assertions for the public consumer dependency profiles.

use std::path::Path;
use std::process::Command;

fn direct_tree(features: &str) -> String {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let manifest = match manifest.to_str() {
        Some(manifest) => manifest,
        None => panic!("manifest path is valid UTF-8"),
    };
    let output = match Command::new(cargo)
        .args([
            "tree",
            "--manifest-path",
            manifest,
            "-p",
            "phoxal",
            "--no-default-features",
            "--features",
            features,
            "--edges",
            "normal",
            "--depth",
            "1",
            "--prefix",
            "none",
        ])
        .output()
    {
        Ok(output) => output,
        Err(error) => panic!("cargo tree starts: {error}"),
    };
    assert!(
        output.status.success(),
        "cargo tree {features:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    match String::from_utf8(output.stdout) {
        Ok(output) => output,
        Err(error) => panic!("cargo tree output is UTF-8: {error}"),
    }
}

fn has_direct_package(tree: &str, package: &str) -> bool {
    tree.lines().any(|line| {
        line.split_whitespace()
            .next()
            .is_some_and(|name| name == package)
    })
}

fn assert_direct_packages(tree: &str, present: &[&str], absent: &[&str]) {
    for package in present {
        assert!(
            has_direct_package(tree, package),
            "{package} missing from direct tree:\n{tree}"
        );
    }
    for package in absent {
        assert!(
            !has_direct_package(tree, package),
            "{package} unexpectedly present in direct tree:\n{tree}"
        );
    }
}

#[test]
fn contract_profile_is_transport_free() {
    let tree = direct_tree("contract");
    assert_direct_packages(
        &tree,
        &["phoxal"],
        &["tokio", "tokio-util", "zenoh", "phoxal-macros", "clap"],
    );
}

#[test]
fn session_profile_is_public_client_only() {
    let tree = direct_tree("session");
    assert_direct_packages(
        &tree,
        &["phoxal", "prost", "tokio", "tokio-util", "zenoh"],
        &[
            "phoxal-macros",
            "clap",
            "tempfile",
            "tracing-subscriber",
            "system_shutdown",
        ],
    );
}

#[test]
#[cfg(feature = "runtime")]
fn runtime_profile_retains_runner_dependencies() {
    let tree = direct_tree("runtime");
    assert_direct_packages(&tree, &["phoxal-macros", "clap", "tokio", "zenoh"], &[]);
}
