//! What the workspace's crates are allowed to depend on: the direction of the
//! public library graph, the edges no canonical crate may ever grow, and the
//! Zenoh feature set every binary links.
//!
//! The dependency facts that need the framework *linked* are not here and
//! cannot be: the wire protocol version the transport actually speaks is proved
//! against the owning protocol contract, while project preparation and source
//! validation are proved in `phoxal-project`'s own tests.

use std::fs;

use anyhow::{Context, Result};
use cargo_metadata::{DependencyKind, MetadataCommand};

use super::framework_executable::SPECS;
use super::{Subject, Violation, is_library_package};

/// Every edge the published library graph may carry, and no other.
///
/// Stated as the complete list rather than as a set of bans: a graph that is
/// only forbidden from growing particular edges says nothing about the one it
/// grows next. The reusable library graph is explicit. The facade owns the
/// optional typed port surface and the `phoxal::build` authoring helper; both
/// are re-exported over the canonical `phoxal-port` and `phoxal-build` owner
/// crates. Service-owned contract crates consume the typed port vocabulary
/// through the `phoxal` facade's `port` feature, never directly. Build-time
/// generators live behind `phoxal::build`; the generator's host build-script
/// edge to `phoxal-build` stays inside Cargo's build-dependency graph and is
/// not a normal runtime edge.
///
/// `phoxal-mujoco` is a framework library that depends on the facade's port
/// surface plus its owned component contract crates; component crates
/// themselves are not libraries for this rule and so are checked elsewhere.
const ALLOWED_LIBRARY_EDGES: [(&str, &str); 17] = [
    // The facade re-exports typed ports, the typed-port macros, and the
    // internal build helper behind the optional `port`/`build`/`runtime`
    // features. These are the three edges the facade grows itself.
    ("phoxal", "phoxal-macros"),
    ("phoxal", "phoxal-port"),
    ("phoxal", "phoxal-build"),
    // Framework host binaries that own the rest of the runtime / authoring
    // graph.
    ("phoxal-supervisor", "phoxal"),
    ("phoxal-project", "phoxal"),
    // The native engine binding consumes the facade's port vocabulary.
    ("phoxal-mujoco", "phoxal"),
    // Service-owned contract libraries reach the facade through its `port`
    // feature; they never declare `phoxal-port` directly.
    ("phoxal-service-motion", "phoxal"),
    ("phoxal-service-navigation", "phoxal"),
    ("phoxal-service-kinematics", "phoxal"),
    ("phoxal-service-world", "phoxal"),
    ("phoxal-service-safety", "phoxal"),
    // The shared robotics vocabulary crate re-exports typed ports through the
    // facade so generated descriptors and inert port types resolve uniformly.
    ("phoxal-robotics", "phoxal"),
    // Cross-service and shared-vocabulary edges retained from the prior
    // design. Motion owns the protective-constraint payload, so Safety depends
    // on Motion for both `MotionStatus` and the canonical constraint input.
    ("phoxal-service-kinematics", "phoxal-robotics"),
    ("phoxal-service-world", "phoxal-service-kinematics"),
    ("phoxal-service-safety", "phoxal-robotics"),
    ("phoxal-service-safety", "phoxal-service-motion"),
    ("phoxal-service-safety", "phoxal-service-world"),
];

/// The edges a canonical crate may never grow, whatever the dependency kind.
///
/// The direction stays one-way: `phoxal-macros` is expanded by `phoxal` and
/// knows nothing about it, and neither library reaches for the CLI that
/// consumes them.
const FORBIDDEN_EDGES: [(&str, &[&str]); 2] = [
    ("phoxal-macros", &["phoxal", "phoxal-cli"]),
    ("phoxal", &["phoxal-cli"]),
];

/// The framework library packages that stopped existing when the framework
/// became one library.
///
/// They are modules of `phoxal` now, and their last published versions stay on
/// crates.io forever. So a dependency on one of them still resolves, still
/// compiles, and silently reintroduces a second copy of the model, the bus
/// vocabulary or the wire contracts - which is a second compatibility identity
/// in a product that has exactly one. Nothing may depend on them again, whatever
/// the dependency kind.
const RETIRED_LIBRARIES: [&str; 6] = [
    "phoxal-protocol",
    "phoxal-bus",
    "phoxal-bundle",
    "phoxal-model",
    "phoxal-manifest",
    "phoxal-runtime-contract",
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

pub(super) fn canonical_crates_and_the_framework_executable_keep_forbidden_edges_absent(
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
    Ok(violations)
}

/// The retired library packages stay retired.
pub(super) fn retired_framework_libraries_stay_absent(subject: &Subject) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for package in &subject.members.packages {
        for dependency in &package.dependencies {
            if RETIRED_LIBRARIES.contains(&dependency.name.as_str()) {
                violations.push(Violation::new(format!(
                    "{} -> {} ({:?}); that package is a module of {} now, and depending on its \
                     last published version would compile a second copy of contracts this package \
                     owns exactly one of",
                    package.name,
                    dependency.name,
                    dependency.kind,
                    super::FACADE
                )));
            }
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
    if direct_zenoh_dependencies == 0 {
        violations.push(Violation::new("no transport dependency was inspected"));
    }
    Ok(violations)
}
