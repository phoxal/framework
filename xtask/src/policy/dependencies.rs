//! What the workspace's crates are allowed to depend on: the direction of the
//! public library graph, the edges no canonical crate may ever grow, and the
//! Zenoh feature set every binary links.
//!
//! The two dependency facts that need the framework *linked* are not here and
//! cannot be: the wire protocol version the transport actually speaks is proved
//! against `phoxal-bus`'s own declared contract surface, in that crate's tests,
//! and the build-requirement union CI installs is proved against
//! `phoxal-manifest`'s own reader, in that crate's tests.

use std::fs;

use anyhow::{Context, Result};
use cargo_metadata::{DependencyKind, MetadataCommand};

use super::artifact::ArtifactKind;
use super::framework_executable::SPECS;
use super::{Subject, Violation, is_library_package};

/// Every edge the published library graph may carry, and no other.
///
/// Stated as the complete list rather than as a set of bans: a graph that is
/// only forbidden from growing particular edges says nothing about the one it
/// grows next.
const ALLOWED_LIBRARY_EDGES: [(&str, &str); 18] = [
    ("phoxal", "phoxal-protocol"),
    ("phoxal", "phoxal-bus"),
    ("phoxal", "phoxal-bundle"),
    ("phoxal", "phoxal-macros"),
    ("phoxal", "phoxal-model"),
    ("phoxal", "phoxal-runtime-contract"),
    ("phoxal-protocol", "phoxal-bus"),
    ("phoxal-protocol", "phoxal-macros"),
    // The supervisor's identity endpoint answers with the compiled robot
    // itself, so the manifest document is a protocol body. The direction stays
    // one-way: `phoxal-model` knows nothing about the wire it travels on.
    ("phoxal-protocol", "phoxal-model"),
    ("phoxal-protocol", "phoxal-runtime-contract"),
    // Every crate that owns a wire or document contract derives the wire
    // shape of its own declarations, so the derive reaches each of them.
    // The direction stays one-way: `phoxal-macros` depends on none of them.
    // `phoxal-bundle` is not among them: it owns the bundle layout, and the one
    // document it reads is declared by `phoxal-model`.
    ("phoxal-bundle", "phoxal-model"),
    ("phoxal-bundle", "phoxal-runtime-contract"),
    ("phoxal-bus", "phoxal-macros"),
    ("phoxal-bus", "phoxal-runtime-contract"),
    ("phoxal-manifest", "phoxal-model"),
    ("phoxal-manifest", "phoxal-runtime-contract"),
    ("phoxal-model", "phoxal-macros"),
    // Canonical model identities originate in the process-contract layer
    // so runtime consumers do not need the authored model crate.
    ("phoxal-model", "phoxal-runtime-contract"),
];

/// The edges a canonical crate may never grow, whatever the dependency kind.
const FORBIDDEN_EDGES: [(&str, &[&str]); 4] = [
    (
        "phoxal-model",
        &[
            "phoxal-manifest",
            "phoxal",
            "phoxal-bus",
            "tokio",
            "clap",
            "zenoh",
            "serde_yaml",
            "urdf-rs",
            "anyhow",
        ],
    ),
    ("phoxal-manifest", &["phoxal", "phoxal-bus", "phoxal-cli"]),
    (
        "phoxal-bundle",
        &[
            "phoxal-manifest",
            "phoxal",
            "phoxal-cli",
            "serde_yaml",
            "urdf-rs",
        ],
    ),
    (
        "phoxal-runtime-contract",
        &["phoxal", "phoxal-model", "tokio", "clap", "zenoh"],
    ),
];

pub(super) fn public_library_dependency_direction_is_exact(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    let mut actual = Vec::new();
    for package in subject
        .members
        .packages
        .iter()
        .filter(|package| is_library_package(package.name.as_str()))
    {
        for dependency in package.dependencies.iter().filter(|dependency| {
            dependency.kind == DependencyKind::Normal
                && is_library_package(dependency.name.as_str())
        }) {
            actual.push((package.name.as_str(), dependency.name.as_str()));
        }
    }

    let mut violations = Vec::new();
    for edge in &actual {
        if !ALLOWED_LIBRARY_EDGES.contains(edge) {
            violations.push(Violation::new(format!(
                "{} -> {} is not an allowed public library edge",
                edge.0, edge.1
            )));
        }
    }
    for edge in ALLOWED_LIBRARY_EDGES {
        if !actual.contains(&edge) {
            violations.push(Violation::new(format!(
                "{} -> {} is an allowed public library edge the workspace no longer declares",
                edge.0, edge.1
            )));
        }
    }
    Ok(violations)
}

pub(super) fn canonical_crates_and_official_participants_keep_forbidden_edges_absent(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for (package_name, forbidden) in FORBIDDEN_EDGES {
        let package = subject
            .members
            .packages
            .iter()
            .find(|package| package.name.as_str() == package_name)
            .with_context(|| format!("missing package {package_name}"))?;
        for dependency in &package.dependencies {
            if forbidden.contains(&dependency.name.as_str()) {
                violations.push(Violation::new(format!(
                    "{} -> {} ({:?})",
                    package.name, dependency.name, dependency.kind
                )));
            }
        }
    }

    for spec in SPECS {
        let package = subject
            .members
            .packages
            .iter()
            .find(|package| package.name.as_str() == spec.package_name())
            .with_context(|| format!("missing package {}", spec.package_name()))?;
        for dependency in &package.dependencies {
            if spec
                .forbidden_dependencies()
                .contains(&dependency.name.as_str())
            {
                violations.push(Violation::new(format!(
                    "{} -> {} ({:?})",
                    package.name, dependency.name, dependency.kind
                )));
            }
        }
    }

    // An official artifact consumes a finalized bundle; the authored-manifest
    // reader stops at bundle compilation and never links into a participant.
    for package in &subject.members.packages {
        let manifest = package.manifest_path.as_std_path();
        let relative = manifest.strip_prefix(&subject.root).unwrap_or(manifest);
        let official = relative
            .components()
            .next()
            .and_then(|value| value.as_os_str().to_str())
            .is_some_and(|top_level| ArtifactKind::try_from(top_level).is_ok());
        if official
            && package
                .dependencies
                .iter()
                .any(|dependency| dependency.name.as_str() == "phoxal-manifest")
        {
            violations.push(Violation::new(format!(
                "{} -> phoxal-manifest",
                package.name
            )));
        }
    }
    Ok(violations)
}

pub(super) fn zenoh_dependency_profiles_keep_transport_compression_disabled(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    let workspace_manifest = subject.root.join("Cargo.toml");
    let document = fs::read_to_string(&workspace_manifest)?
        .parse::<toml_edit::DocumentMut>()
        .context("workspace Cargo.toml is invalid")?;
    let dependency = "zenoh";

    let mut violations = Vec::new();
    if document["workspace"]["dependencies"][dependency]["default-features"].as_bool()
        != Some(false)
    {
        violations.push(Violation::new(format!(
            "{dependency} must disable Zenoh default features while RUSTSEC-2026-0041 is ignored"
        )));
    }
    let features = document["workspace"]["dependencies"][dependency]["features"]
        .as_array()
        .with_context(|| format!("{dependency} must declare an explicit feature list"))?;
    if features
        .iter()
        .any(|feature| feature.as_str() == Some("transport_compression"))
    {
        violations.push(Violation::new(format!(
            "{dependency} must keep transport_compression disabled while RUSTSEC-2026-0041 is \
             ignored"
        )));
    }

    // The resolved graph, not the member list: a workspace crate that reached
    // Zenoh through a renamed or optional declaration would be invisible to the
    // manifest read above.
    let metadata = MetadataCommand::new()
        .manifest_path(&workspace_manifest)
        .exec()
        .context("workspace cargo metadata failed")?;
    let mut direct_zenoh_dependencies = 0;
    for package in metadata
        .packages
        .iter()
        .filter(|package| package.source.is_none())
    {
        for dependency in package
            .dependencies
            .iter()
            .filter(|dependency| dependency.name == "zenoh")
        {
            direct_zenoh_dependencies += 1;
            if dependency.uses_default_features {
                violations.push(Violation::new(format!(
                    "{} enables Zenoh default features",
                    package.name
                )));
            }
            if dependency
                .features
                .iter()
                .any(|feature| feature == "transport_compression")
            {
                violations.push(Violation::new(format!(
                    "{} enables Zenoh transport_compression",
                    package.name
                )));
            }
        }
    }
    if direct_zenoh_dependencies != 1 {
        violations.push(Violation::new(format!(
            "the workspace declares {direct_zenoh_dependencies} direct Zenoh dependencies; every \
             one must be covered by this guard, and exactly one crate links the transport"
        )));
    }
    Ok(violations)
}
