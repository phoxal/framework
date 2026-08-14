//! What the workspace's crates are allowed to depend on: the direction of the
//! public library graph, the edges no canonical crate may ever grow, the
//! system packages CI installs, the Zenoh feature set every binary links, and
//! the transport wire version the frozen bootstrap stands on.

use std::fs;

use anyhow::{Context, Result};
use cargo_metadata::{DependencyKind, MetadataCommand};
use workspace_policy::artifact::ArtifactKind;
use workspace_policy::{is_library_package, workspace_root};

fn workspace_metadata() -> Result<cargo_metadata::Metadata> {
    MetadataCommand::new()
        .manifest_path(workspace_root()?.join("Cargo.toml"))
        .no_deps()
        .exec()
        .context("failed to read workspace metadata")
}

/// The apt packages CI installs are not a workflow author's choice: they are
/// the union of what the workspace's manifests declare they need.
#[test]
fn phoxal_metadata_namespace_is_valid_in_every_workspace_manifest() -> Result<()> {
    let workspace_root = workspace_root()?;
    let metadata = workspace_metadata()?;

    let mut declared_packages = std::collections::BTreeSet::new();
    for package in metadata.workspace_packages() {
        let path = package.manifest_path.clone().into_std_path_buf();
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let requirements = phoxal_manifest::build_requirements::BuildRequirements::from_manifest(
            &source,
            &path.display().to_string(),
        )?;
        declared_packages.extend(requirements.apt.iter().cloned());
    }

    // A workspace that declares nothing must also install nothing: the
    // workflows carry no line at all rather than an empty one, so an
    // absent declaration reads as the empty union.
    let declared = declared_packages.into_iter().collect::<Vec<_>>();
    let declared_in = |workflow: &str, prefix: &str| -> Result<Vec<String>> {
        let source = fs::read_to_string(workspace_root.join(workflow))?;
        Ok(source
            .lines()
            .find_map(|line| line.trim().strip_prefix(prefix))
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect())
    };
    assert_eq!(
        declared_in(".github/workflows/ci.yml", "system-packages:")?,
        declared,
        "CI system-packages must equal the manifest-declared union"
    );
    assert_eq!(
        declared_in(
            ".github/workflows/release-plz.yml",
            "sudo apt-get install -y"
        )?,
        declared,
        "release packaging dependencies must equal the manifest-declared union"
    );
    Ok(())
}

#[test]
fn public_library_dependency_direction_is_exact() -> Result<()> {
    let metadata = workspace_metadata()?;
    let allowed = [
        ("phoxal", "phoxal-protocol"),
        ("phoxal", "phoxal-bus"),
        ("phoxal", "phoxal-bundle"),
        ("phoxal", "phoxal-macros"),
        ("phoxal", "phoxal-model"),
        ("phoxal", "phoxal-runtime-contract"),
        ("phoxal-protocol", "phoxal-bus"),
        ("phoxal-protocol", "phoxal-macros"),
        ("phoxal-protocol", "phoxal-runtime-contract"),
        // Every crate that owns a wire or document contract derives the wire
        // shape of its own declarations, so the derive reaches each of them.
        // The direction stays one-way: `phoxal-macros` depends on none of them.
        ("phoxal-bundle", "phoxal-macros"),
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
    let mut actual = Vec::new();
    for package in metadata
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
    actual.sort_unstable();
    let mut expected = allowed.to_vec();
    expected.sort_unstable();
    assert_eq!(
        actual, expected,
        "public library dependency direction drifted"
    );
    Ok(())
}

#[test]
fn canonical_crates_and_official_participants_keep_forbidden_edges_absent() -> Result<()> {
    let workspace_root = workspace_root()?;
    let metadata = workspace_metadata()?;
    let bans: &[(&str, &[&str])] = &[
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
    let mut violations = Vec::new();
    for (package_name, forbidden) in bans {
        let package = metadata
            .packages
            .iter()
            .find(|package| package.name.as_str() == *package_name)
            .with_context(|| format!("missing package {package_name}"))?;
        for dependency in &package.dependencies {
            if forbidden.contains(&dependency.name.as_str()) {
                violations.push(format!(
                    "{} -> {} ({:?})",
                    package.name, dependency.name, dependency.kind
                ));
            }
        }
    }

    for package in &metadata.packages {
        let manifest = package.manifest_path.as_std_path();
        let relative = manifest.strip_prefix(workspace_root).unwrap_or(manifest);
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
            violations.push(format!("{} -> phoxal-manifest", package.name));
        }
    }

    assert!(
        violations.is_empty(),
        "forbidden dependency edges:\n{}",
        violations.join("\n")
    );
    Ok(())
}

#[test]
fn zenoh_dependency_profiles_keep_transport_compression_disabled() -> Result<()> {
    let workspace_manifest = workspace_root()?.join("Cargo.toml");
    let document = fs::read_to_string(&workspace_manifest)?
        .parse::<toml_edit::DocumentMut>()
        .context("workspace Cargo.toml is invalid")?;
    let dependency = "zenoh";
    assert_eq!(
        document["workspace"]["dependencies"][dependency]["default-features"].as_bool(),
        Some(false),
        "{dependency} must disable Zenoh default features while RUSTSEC-2026-0041 is ignored"
    );
    let features = document["workspace"]["dependencies"][dependency]["features"]
        .as_array()
        .with_context(|| format!("{dependency} must declare an explicit feature list"))?;
    assert!(
        !features
            .iter()
            .any(|feature| feature.as_str() == Some("transport_compression")),
        "{dependency} must keep transport_compression disabled while RUSTSEC-2026-0041 is ignored"
    );

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
            assert!(
                !dependency.uses_default_features,
                "{} enables Zenoh default features",
                package.name
            );
            assert!(
                !dependency
                    .features
                    .iter()
                    .any(|feature| feature == "transport_compression"),
                "{} enables Zenoh transport_compression",
                package.name
            );
        }
    }
    assert_eq!(
        direct_zenoh_dependencies, 1,
        "every direct Zenoh dependency must be covered by this guard"
    );
    Ok(())
}

/// The Zenoh wire protocol version `phoxal-bus` freezes is the one the linked
/// transport actually speaks.
///
/// The bus declares the version in its own contract surface, because the
/// compatibility checker reads a declared boundary and `zenoh-protocol` is an
/// internal crate of the transport that no Phoxal crate depends on. This is
/// what stops that declaration going stale behind a Zenoh upgrade: the resolved
/// `zenoh-protocol` is read from the source Cargo actually compiles, and its
/// own constant is compared against what the bus declares.
///
/// A Zenoh release that moves this number is a bootstrap-breaking event and not
/// a routine dependency bump: peers that disagree on it never form a session,
/// so nothing above it gets the chance to report the disagreement. See
/// xtask/README.md "When a gate fails", rule 3 "A frozen bootstrap fact
/// drifted".
#[test]
fn the_frozen_zenoh_wire_protocol_version_is_the_one_the_transport_speaks() -> Result<()> {
    let metadata = MetadataCommand::new()
        .manifest_path(workspace_root()?.join("Cargo.toml"))
        .exec()
        .context("workspace cargo metadata failed")?;
    let resolved = metadata
        .packages
        .iter()
        .filter(|package| package.name.as_str() == "zenoh-protocol")
        .collect::<Vec<_>>();
    assert_eq!(
        resolved.len(),
        1,
        "the workspace must link exactly one zenoh-protocol, or the version read here is not the \
         one the bus speaks: {:?}",
        resolved
            .iter()
            .map(|package| package.version.to_string())
            .collect::<Vec<_>>()
    );
    let source_root = resolved[0]
        .manifest_path
        .parent()
        .context("the resolved zenoh-protocol has no source directory")?;
    let source = fs::read_to_string(source_root.join("src/lib.rs").as_std_path())
        .context("the resolved zenoh-protocol source could not be read")?;

    let literal = source
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("pub const VERSION: u8 = ")?
                .strip_suffix(';')
        })
        .context(
            "zenoh-protocol no longer states its wire protocol version as `pub const VERSION: \
             u8`. That is itself a transport change worth reading before it ships: find where the \
             version now lives, then follow xtask/README.md \"When a gate fails\", rule 3.",
        )?;
    let spoken = match literal.trim().strip_prefix("0x") {
        Some(hexadecimal) => u8::from_str_radix(hexadecimal, 16),
        None => literal.trim().parse::<u8>(),
    }
    .with_context(|| format!("zenoh-protocol states an unreadable wire version `{literal}`"))?;

    let declared =
        format!(r#"{{"name":"zenoh-wire-protocol","record":"identifier","value":"{spoken}"}}"#);
    let surface = phoxal_bus::__compat::contract_surface();
    assert!(
        surface.contains(&declared),
        "phoxal-bus freezes a Zenoh wire protocol version the linked transport does not speak. \
         The transport says {spoken}. This is a bootstrap-breaking transport change: do not edit \
         the pin to match. See xtask/README.md \"When a gate fails\", rule 3.\n{surface}"
    );
    Ok(())
}
