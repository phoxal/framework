//! Workspace policy: the rules the framework workspace must obey as a whole,
//! and the tests that enforce them.
//!
//! No single crate owns these facts - that a package's directory, name and
//! `publish` field agree, that the zenoh dependency set keeps transport
//! compression disabled - so they live
//! here, in a crate whose only purpose is to be a test target under
//! `cargo test --workspace`.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
#[cfg(test)]
use cargo_metadata::DependencyKind;
use cargo_metadata::{MetadataCommand, Target, TargetKind};
use serde::{Deserialize, Serialize};

const LIBRARY_CRATE_DIRS: [&str; 7] = [
    "phoxal",
    "phoxal-api",
    "phoxal-bus",
    "phoxal-macros",
    "phoxal-manifest",
    "phoxal-model",
    "phoxal-runtime-contract",
];
const EXCLUDED_TOP_LEVEL_DIRS: [&str; 2] = ["workspace-policy", "fixture"];

/// Official Phoxal packages always use this provider segment in their public
/// `package` identity (`phoxal/<name>`). Third-party packages use their own
/// provider segment; the grammar has no other official provider today.
pub const PHOXAL_PROVIDER: &str = "phoxal";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Service,
    /// A component crate: the package-named driver binary plus its assets
    /// (`component.yaml`, `simulation.yaml`, `structure.urdf`, and `meshes/`
    /// if present) in one package. Like every official artifact package, it
    /// has exactly one binary target and no library target. `cargo package`
    /// picks the assets up by default, so they need no inclusion rules.
    /// Discovered from `component/<id>/Cargo.toml`.
    Component,
    Simulator,
}

impl fmt::Display for ArtifactKind {
    /// Renders the kind as its top-level directory segment, the same token
    /// `artifact_kind_from_directory` parses.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.pad(match self {
            ArtifactKind::Service => "service",
            ArtifactKind::Component => "component",
            ArtifactKind::Simulator => "simulator",
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct OfficialArtifact {
    /// The provider-qualified public identity, e.g. `phoxal/component-ddsm115`.
    /// This is the sole public identity; asset filenames are filesystem-safe
    /// projections of it.
    pub package: String,
    /// The Cargo crate name backing this package (e.g. `phoxal-component-ddsm115`).
    pub package_name: String,
    pub kind: ArtifactKind,
    pub version: String,
    pub crate_dir: PathBuf,
    /// The serialized release binary identity. Discovery guarantees it equals
    /// `package_name`; it remains explicit for release consumers.
    pub bin_name: String,
    pub id: String,
}

#[derive(Debug)]
pub struct Workspace {
    root: PathBuf,
    target_dir: PathBuf,
    official_artifacts: Vec<OfficialArtifact>,
}

impl Workspace {
    pub fn discover() -> Result<Self> {
        Self::discover_with(&mut MetadataCommand::new())
    }

    fn discover_with(command: &mut MetadataCommand) -> Result<Self> {
        let metadata = command
            .no_deps()
            .exec()
            .context("failed to read cargo metadata")?;
        let root = metadata.workspace_root.clone().into_std_path_buf();
        let target_dir = metadata.target_directory.clone().into_std_path_buf();
        let mut official_artifacts = Vec::new();

        for package in metadata.workspace_packages() {
            let package_name = package.name.to_string();
            let manifest_path = package.manifest_path.clone().into_std_path_buf();
            let manifest = classify_manifest_path(&root, &manifest_path)
                .with_context(|| format!("failed to classify {}", manifest_path.display()))?;
            let ManifestClassification::Artifact { kind, id } = manifest else {
                if let Some((prefix_kind, prefix_id)) = classify_package_prefix(&package_name) {
                    bail!(
                        "{package_name} uses the {prefix_kind} artifact package prefix with id \
                         '{prefix_id}' but its manifest path {} is outside the exact \
                         directory grammar for that kind",
                        relative_display(&root, &manifest_path)
                    );
                }
                continue;
            };

            validate_package_name(&package_name, kind, &id, &root, &manifest_path)?;
            validate_artifact_publish(
                &package_name,
                package.publish.as_deref(),
                &root,
                &manifest_path,
            )?;
            let crate_dir = manifest_path
                .parent()
                .with_context(|| format!("{package_name} manifest has no parent directory"))?
                .to_path_buf();
            let bin_name = validate_artifact_targets(&package_name, &package.targets)?;

            official_artifacts.push(OfficialArtifact {
                package: package_identity(kind, &id),
                package_name,
                kind,
                version: package.version.to_string(),
                crate_dir,
                bin_name,
                id,
            });
        }

        official_artifacts.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.package.cmp(&right.package))
        });

        Ok(Self {
            root,
            target_dir,
            official_artifacts,
        })
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    pub fn target_dir(&self) -> &PathBuf {
        &self.target_dir
    }

    pub fn official_artifacts(&self) -> &[OfficialArtifact] {
        &self.official_artifacts
    }
}

/// The provider-qualified public identity for a discovered package
/// (`phoxal/service-drive`, `phoxal/component-ddsm115`, ...). No `kind`
/// suffix - a component crate is one package carrying both its binary and
/// asset outputs (design doc §9).
pub fn package_identity(kind: ArtifactKind, id: &str) -> String {
    format!("{PHOXAL_PROVIDER}/{}", package_name_segment(kind, id))
}

/// Enforces the executable-only target convention for every official artifact
/// package: exactly one package-named binary and no library target. Components
/// ship their authored assets beside that binary in the same archive; they do
/// not need a Cargo library target for discovery or packaging.
fn validate_artifact_targets(package_name: &str, targets: &[Target]) -> Result<String> {
    if let Some((target, target_kind)) = targets
        .iter()
        .find_map(|target| unsupported_target_kind(target).map(|kind| (target, kind)))
    {
        if is_library_target_kind(target_kind) {
            bail!(
                "{package_name} is an official artifact package but target '{}' has library kind '{target_kind}'",
                target.name
            );
        }
        bail!(
            "{package_name} is an official artifact package but target '{}' has unsupported target kind '{target_kind}'; expected bin, test, bench, example, or custom-build",
            target.name
        );
    }

    let binary_targets: Vec<_> = targets
        .iter()
        .filter(|target| target.is_kind(TargetKind::Bin))
        .collect();

    let [binary_target] = binary_targets.as_slice() else {
        bail!(
            "{package_name} is an official artifact package but has {} binary targets; expected exactly one",
            binary_targets.len()
        );
    };
    if binary_target.name != package_name {
        bail!(
            "{package_name} is an official artifact package but its only binary target is '{}'; expected '{package_name}'",
            binary_target.name
        );
    }

    Ok(package_name.to_owned())
}

fn unsupported_target_kind(target: &Target) -> Option<&TargetKind> {
    target
        .kind
        .iter()
        .find(|kind| !is_allowed_artifact_target_kind(kind))
}

fn is_allowed_artifact_target_kind(kind: &TargetKind) -> bool {
    matches!(
        kind,
        TargetKind::Bin
            | TargetKind::Test
            | TargetKind::Bench
            | TargetKind::Example
            | TargetKind::CustomBuild
    )
}

fn is_library_target_kind(kind: &TargetKind) -> bool {
    matches!(
        kind,
        TargetKind::Lib
            | TargetKind::RLib
            | TargetKind::DyLib
            | TargetKind::CDyLib
            | TargetKind::StaticLib
            | TargetKind::ProcMacro
    )
}

fn package_name_segment(kind: ArtifactKind, id: &str) -> String {
    match kind {
        ArtifactKind::Service => format!("service-{id}"),
        ArtifactKind::Component => format!("component-{id}"),
        ArtifactKind::Simulator => format!("simulator-{id}"),
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ManifestClassification {
    Artifact { kind: ArtifactKind, id: String },
    Excluded,
    NonArtifact,
}

fn classify_manifest_path(root: &Path, manifest_path: &Path) -> Result<ManifestClassification> {
    let relative = manifest_path.strip_prefix(root).with_context(|| {
        format!(
            "{} is not under workspace root {}",
            manifest_path.display(),
            root.display()
        )
    })?;
    let components = path_components(relative)?;
    if components.is_empty() {
        bail!("manifest path {} is empty", relative.display());
    }

    let top_level = components[0];
    if EXCLUDED_TOP_LEVEL_DIRS.contains(&top_level) {
        return Ok(ManifestClassification::Excluded);
    }
    if LIBRARY_CRATE_DIRS.contains(&top_level) && components.as_slice() == [top_level, "Cargo.toml"]
    {
        return Ok(ManifestClassification::Excluded);
    }

    let Some(kind) = artifact_kind_from_directory(top_level) else {
        return Ok(ManifestClassification::NonArtifact);
    };
    if components.len() != 3 || components[2] != "Cargo.toml" || components[1].is_empty() {
        bail!(
            "workspace package manifest {} is nested under artifact root '{}'; official artifacts \
             must live exactly at {{service,component,simulator}}/<id>/Cargo.toml",
            relative.display(),
            top_level
        );
    }

    Ok(ManifestClassification::Artifact {
        kind,
        id: components[1].to_string(),
    })
}

fn path_components(path: &Path) -> Result<Vec<&str>> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .with_context(|| format!("path component in {} is not UTF-8", path.display())),
            _ => bail!("path {} contains non-normal component", path.display()),
        })
        .collect()
}

fn artifact_kind_from_directory(directory: &str) -> Option<ArtifactKind> {
    match directory {
        "service" => Some(ArtifactKind::Service),
        "component" => Some(ArtifactKind::Component),
        "simulator" => Some(ArtifactKind::Simulator),
        _ => None,
    }
}

fn validate_package_name(
    package_name: &str,
    kind: ArtifactKind,
    id: &str,
    root: &Path,
    manifest_path: &Path,
) -> Result<()> {
    let expected = expected_package_name(kind, id);
    if package_name != expected {
        bail!(
            "{} is in artifact directory for {} but package.name is '{}'; expected '{}'",
            relative_display(root, manifest_path),
            kind,
            package_name,
            expected
        );
    }
    Ok(())
}

/// Every official executable publishes to the `phoxal` registry and only there.
///
/// This is the Cargo-side guard against an accidental crates.io publication
/// (organization#951, Decision 2). It is also what keeps release-plz able to
/// see these packages at all: a `publish = false` package is invisible to it,
/// and an invisible package cannot bump the train when it changes - the defect
/// Decision 11 exists to fix.
fn validate_artifact_publish(
    package_name: &str,
    publish: Option<&[String]>,
    root: &Path,
    manifest_path: &Path,
) -> Result<()> {
    let declared = publish.unwrap_or_default();
    if declared != [PHOXAL_PROVIDER] {
        bail!(
            "{package_name} is an official executable but {} does not set \
             publish = [\"{PHOXAL_PROVIDER}\"]; executables publish to the static \
             {PHOXAL_PROVIDER} registry and never to crates.io, and release-plz must be able \
             to see them so their changes cut trains. Found: {}",
            relative_display(root, manifest_path),
            if publish.is_none() {
                "no publish field (defaults to crates.io)".to_string()
            } else {
                format!("publish = {declared:?}")
            }
        );
    }
    Ok(())
}

/// The Cargo `package.name` a crate-backed artifact must use for its kind + id.
fn expected_package_name(kind: ArtifactKind, id: &str) -> String {
    match kind {
        ArtifactKind::Service => format!("phoxal-service-{id}"),
        ArtifactKind::Component => format!("phoxal-component-{id}"),
        ArtifactKind::Simulator => format!("phoxal-simulator-{id}"),
    }
}

fn classify_package_prefix(package_name: &str) -> Option<(ArtifactKind, String)> {
    package_name
        .strip_prefix("phoxal-service-")
        .map(|id| (ArtifactKind::Service, id.to_string()))
        .or_else(|| {
            package_name
                .strip_prefix("phoxal-component-")
                .map(|id| (ArtifactKind::Component, id.to_string()))
        })
        .or_else(|| {
            package_name
                .strip_prefix("phoxal-simulator-")
                .map(|id| (ArtifactKind::Simulator, id.to_string()))
        })
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;

    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/repo")
    }

    fn classify(relative: &str) -> Result<ManifestClassification> {
        classify_manifest_path(&root(), &root().join(relative))
    }

    fn workspace_root() -> Result<&'static Path> {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .context("workspace-policy manifest directory has no workspace parent")
    }

    #[test]
    fn phoxal_metadata_namespace_is_valid_in_every_workspace_manifest() -> Result<()> {
        let workspace_root = workspace_root()?;
        let metadata = MetadataCommand::new()
            .manifest_path(workspace_root.join("Cargo.toml"))
            .no_deps()
            .exec()
            .context("failed to read workspace metadata")?;

        let mut declared_packages = std::collections::BTreeSet::new();
        for package in metadata.workspace_packages() {
            let path = package.manifest_path.clone().into_std_path_buf();
            let source = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let requirements = phoxal_manifest::build_requirements::requirements_from_manifest(
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
        let workspace_manifest = workspace_root()?.join("Cargo.toml");
        let metadata = MetadataCommand::new()
            .manifest_path(workspace_manifest)
            .no_deps()
            .exec()?;
        let libraries = [
            "phoxal",
            "phoxal-api",
            "phoxal-bus",
            "phoxal-macros",
            "phoxal-manifest",
            "phoxal-model",
            "phoxal-runtime-contract",
        ];
        let allowed = [
            ("phoxal", "phoxal-api"),
            ("phoxal", "phoxal-bus"),
            ("phoxal", "phoxal-macros"),
            // A finalized bundle carries no compiled model document, so the
            // participant runner builds the canonical model from the bundle's
            // own finalized documents through the manifest crate's loader.
            ("phoxal", "phoxal-manifest"),
            ("phoxal", "phoxal-model"),
            ("phoxal", "phoxal-runtime-contract"),
            ("phoxal-api", "phoxal-bus"),
            ("phoxal-api", "phoxal-macros"),
            ("phoxal-bus", "phoxal-runtime-contract"),
            ("phoxal-manifest", "phoxal-model"),
        ];
        let mut actual = Vec::new();
        for package in metadata
            .packages
            .iter()
            .filter(|package| libraries.contains(&package.name.as_str()))
        {
            for dependency in package.dependencies.iter().filter(|dependency| {
                dependency.kind == DependencyKind::Normal
                    && libraries.contains(&dependency.name.as_str())
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
        let workspace_manifest = workspace_root()?.join("Cargo.toml");
        let metadata = MetadataCommand::new()
            .manifest_path(workspace_manifest)
            .no_deps()
            .exec()?;
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
            let relative = manifest.strip_prefix(workspace_root()?).unwrap_or(manifest);
            let mut components = relative.components();
            let top = components
                .next()
                .and_then(|value| value.as_os_str().to_str());
            let second = components
                .next()
                .and_then(|value| value.as_os_str().to_str());
            let official = matches!(
                (top, second),
                (Some("service" | "component" | "simulator"), _)
            );
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

    #[test]
    fn real_workspace_release_scope_is_valid() -> Result<()> {
        let workspace_manifest = workspace_root()?.join("Cargo.toml");
        let workspace =
            Workspace::discover_with(MetadataCommand::new().manifest_path(workspace_manifest))?;

        assert_eq!(workspace.official_artifacts().len(), 18);

        assert_eq!(
            workspace
                .official_artifacts()
                .iter()
                .filter(|artifact| artifact.kind == ArtifactKind::Service)
                .count(),
            12
        );
        let simulators = workspace
            .official_artifacts()
            .iter()
            .filter(|artifact| artifact.kind == ArtifactKind::Simulator)
            .map(|artifact| artifact.package.as_str())
            .collect::<Vec<_>>();
        assert_eq!(simulators, ["phoxal/simulator-webots-controller"]);
        Ok(())
    }

    /// An executable publishes to the `phoxal` registry and nowhere else. The
    /// two rejected cases are the two ways to get this wrong: `publish = false`
    /// hides the package from release-plz so its changes stop cutting trains,
    /// and anything naming crates.io (explicitly or by omission) points the
    /// executables at the wrong channel entirely.
    #[test]
    fn an_executable_publishes_only_to_the_phoxal_registry() {
        let manifest = root().join("simulator/webots-controller/Cargo.toml");
        let check = |publish: Option<&[String]>| {
            validate_artifact_publish(
                "phoxal-simulator-webots-controller",
                publish,
                &root(),
                &manifest,
            )
        };

        check(Some(&["phoxal".to_string()])).expect("publish = [\"phoxal\"] is the one valid form");

        let error = check(Some(&[])).unwrap_err();
        assert!(
            error.to_string().contains("publish = [\"phoxal\"]"),
            "publish = false must be rejected: {error}"
        );

        let error = check(None).unwrap_err();
        assert!(
            error.to_string().contains("defaults to crates.io"),
            "an absent publish field must be rejected: {error}"
        );

        let error = check(Some(&["crates-io".to_string()])).unwrap_err();
        assert!(
            error.to_string().contains("never to crates.io"),
            "crates.io must be rejected: {error}"
        );

        let error = check(Some(&["phoxal".to_string(), "crates-io".to_string()])).unwrap_err();
        assert!(
            error.to_string().contains("publish = [\"phoxal\"]"),
            "a second registry alongside phoxal must be rejected: {error}"
        );
    }

    #[test]
    fn directory_grammar_maps_artifact_kinds() -> Result<()> {
        assert_eq!(
            classify("service/drive/Cargo.toml")?,
            ManifestClassification::Artifact {
                kind: ArtifactKind::Service,
                id: "drive".to_string()
            }
        );
        assert_eq!(
            classify("component/ddsm115/Cargo.toml")?,
            ManifestClassification::Artifact {
                kind: ArtifactKind::Component,
                id: "ddsm115".to_string()
            }
        );
        assert_eq!(
            classify("simulator/webots/Cargo.toml")?,
            ManifestClassification::Artifact {
                kind: ArtifactKind::Simulator,
                id: "webots".to_string()
            }
        );
        Ok(())
    }

    #[test]
    fn nested_crates_under_artifact_roots_are_errors() {
        let err = classify("service/drive/helper/Cargo.toml").unwrap_err();
        assert!(err.to_string().contains("nested under artifact root"));
    }

    #[test]
    fn nested_crates_under_component_are_errors() {
        // The flattened layout (design doc §9) puts the crate directly at
        // `component/<id>/Cargo.toml`; a subdirectory (the old `driver/` split)
        // is no longer valid.
        let err = classify("component/ddsm115/driver/Cargo.toml").unwrap_err();
        assert!(err.to_string().contains("nested under artifact root"));
    }

    #[test]
    fn library_policy_and_fixture_paths_are_excluded() -> Result<()> {
        assert_eq!(
            classify("phoxal/Cargo.toml")?,
            ManifestClassification::Excluded
        );
        assert_eq!(
            classify("workspace-policy/Cargo.toml")?,
            ManifestClassification::Excluded
        );
        assert_eq!(
            classify("fixture/component/foo/Cargo.toml")?,
            ManifestClassification::Excluded
        );
        Ok(())
    }

    #[test]
    fn package_name_must_match_directory_kind_and_id() {
        let err = validate_package_name(
            "phoxal-component-drive-driver",
            ArtifactKind::Service,
            "drive",
            &root(),
            &root().join("service/drive/Cargo.toml"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("expected 'phoxal-service-drive'"));
    }

    #[test]
    fn discovery_enforces_executable_only_official_artifacts() -> Result<()> {
        let workspace_dir = tempfile::tempdir().context("failed to create temp workspace dir")?;
        let root = workspace_dir.path();

        fs::write(
            root.join("Cargo.toml"),
            r#"[workspace]
resolver = "3"
members = ["component/test"]
"#,
        )?;

        let package_dir = root.join("component/test");
        fs::create_dir_all(package_dir.join("src"))?;
        fs::write(
            package_dir.join("src/lib.rs"),
            "//! Deliberately invalid component target fixture.\n",
        )?;
        fs::write(package_dir.join("src/main.rs"), "fn main() {}\n")?;
        fs::write(package_dir.join("src/example.rs"), "fn main() {}\n")?;
        fs::write(package_dir.join("src/test.rs"), "fn main() {}\n")?;
        fs::write(package_dir.join("src/bench.rs"), "fn main() {}\n")?;
        fs::write(package_dir.join("build.rs"), "fn main() {}\n")?;

        let cases = [
            (
                "lib-only",
                "[lib]\npath = \"src/lib.rs\"\n",
                "target 'phoxal_component_test' has library kind 'lib'",
            ),
            (
                "mixed",
                "[lib]\npath = \"src/lib.rs\"\n\n[[bin]]\nname = \"phoxal-component-test\"\npath = \"src/main.rs\"\n",
                "target 'phoxal_component_test' has library kind 'lib'",
            ),
            (
                "rlib",
                "[lib]\npath = \"src/lib.rs\"\ncrate-type = [\"rlib\"]\n\n[[bin]]\nname = \"phoxal-component-test\"\npath = \"src/main.rs\"\n",
                "target 'phoxal_component_test' has library kind 'rlib'",
            ),
            (
                "proc-macro",
                "[lib]\npath = \"src/lib.rs\"\nproc-macro = true\n\n[[bin]]\nname = \"phoxal-component-test\"\npath = \"src/main.rs\"\n",
                "target 'phoxal_component_test' has library kind 'proc-macro'",
            ),
            (
                "zero-bin",
                "[[example]]\nname = \"validation-example\"\npath = \"src/example.rs\"\n",
                "has 0 binary targets; expected exactly one",
            ),
            (
                "wrong-name",
                "[[bin]]\nname = \"wrong-name\"\npath = \"src/main.rs\"\n",
                "its only binary target is 'wrong-name'; expected 'phoxal-component-test'",
            ),
            (
                "multiple-bins",
                "[[bin]]\nname = \"phoxal-component-test\"\npath = \"src/main.rs\"\n\n[[bin]]\nname = \"second\"\npath = \"src/main.rs\"\n",
                "has 2 binary targets; expected exactly one",
            ),
        ];

        for (name, targets, expected_error) in cases {
            fs::write(
                package_dir.join("Cargo.toml"),
                format!(
                    r#"[package]
name = "phoxal-component-test"
version = "0.1.0"
edition = "2024"
license = "AGPL-3.0-only"
publish = ["phoxal"]
description = "Component target validation fixture."
autobins = false
autolib = false

{targets}"#
                ),
            )?;

            let error = Workspace::discover_with(
                MetadataCommand::new().manifest_path(root.join("Cargo.toml")),
            )
            .unwrap_err();
            assert!(
                error.to_string().contains(expected_error),
                "{name} component fixture produced an unexpected error: {error}"
            );
        }

        fs::write(
            package_dir.join("Cargo.toml"),
            r#"[package]
name = "phoxal-component-test"
version = "0.1.0"
edition = "2024"
license = "AGPL-3.0-only"
publish = ["phoxal"]
description = "Component target validation fixture."
autobins = false
autolib = false
autotests = false
autoexamples = false
autobenches = false
build = "build.rs"

[[bin]]
name = "phoxal-component-test"
path = "src/main.rs"

[[test]]
name = "validation-test"
path = "src/test.rs"

[[bench]]
name = "validation-bench"
path = "src/bench.rs"

[[example]]
name = "validation-example"
path = "src/example.rs"
"#,
        )?;
        let workspace = Workspace::discover_with(
            MetadataCommand::new().manifest_path(root.join("Cargo.toml")),
        )?;
        let artifact = workspace.official_artifacts().first().unwrap();
        assert_eq!(artifact.package_name, "phoxal-component-test");
        assert_eq!(artifact.bin_name, "phoxal-component-test");
        Ok(())
    }

    #[test]
    fn official_artifact_target_kind_allowlist_is_complete() {
        for kind in [
            TargetKind::Bin,
            TargetKind::Test,
            TargetKind::Bench,
            TargetKind::Example,
            TargetKind::CustomBuild,
        ] {
            assert!(
                is_allowed_artifact_target_kind(&kind),
                "{kind} must be accepted"
            );
        }
        for kind in [
            TargetKind::Lib,
            TargetKind::RLib,
            TargetKind::DyLib,
            TargetKind::CDyLib,
            TargetKind::StaticLib,
            TargetKind::ProcMacro,
        ] {
            assert!(
                is_library_target_kind(&kind),
                "{kind} must be a library kind"
            );
            assert!(
                !is_allowed_artifact_target_kind(&kind),
                "{kind} must be rejected"
            );
        }
        let unknown = TargetKind::Unknown("future".to_owned());
        assert!(!is_library_target_kind(&unknown));
        assert!(
            !is_allowed_artifact_target_kind(&unknown),
            "unknown target kinds must be rejected"
        );
    }

    #[test]
    fn unknown_target_kind_is_rejected_with_an_unsupported_kind_diagnostic() -> Result<()> {
        let target: Target = serde_json::from_value(serde_json::json!({
            "name": "future-target",
            "kind": ["future"],
            "crate_types": ["bin"],
            "src_path": "/repo/src/main.rs",
            "edition": "2024",
        }))?;

        let error = validate_artifact_targets("phoxal-component-test", &[target]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "phoxal-component-test is an official artifact package but target 'future-target' has unsupported target kind 'future'; expected bin, test, bench, example, or custom-build"
        );
        Ok(())
    }

    /// The two linker-section names a participant's embedded metadata can
    /// live under (`phoxal-macros/src/authoring.rs`'s `link_section_attrs`):
    /// `.phoxal_meta` on ELF, `__phoxal_meta` on Mach-O (`object`'s
    /// [`ObjectSection`] name match ignores the `__DATA` segment
    /// qualifier). Duplicated here rather than imported: there is no
    /// framework-side crate that reads object files today, and the only
    /// other place this exact list lives is `phoxal-cli`'s
    /// `participant_metadata.rs`, in a sibling repository this crate does
    /// not and should not depend on.
    const PARTICIPANT_META_SECTION_NAMES: [&str; 2] = [".phoxal_meta", "__phoxal_meta"];

    /// **What a unit test cannot see: the linker.** `#[used]` keeps a static
    /// alive against this compilation unit's own dead-code elimination, but
    /// ELF `--gc-sections` still drops any section unreachable from `main`
    /// at final link time - which is exactly what silently stripped every
    /// participant's `.phoxal_meta` section before
    /// `Participant::__retain_embedded_metadata` existed (six framework
    /// review rounds missed it because every prior check only read source,
    /// never a built artifact). The only honest check is building a real
    /// participant and inspecting the linked object file, so that is what
    /// this test does: it compiles a throwaway service participant against the
    /// real in-tree `phoxal`/`phoxal-macros` crates in a standalone temp
    /// workspace, then reads the linked binary's metadata section back with the
    /// `object` crate - never executing it, the same "read the bytes, don't
    /// run the binary" discipline `phoxal-cli`'s reader follows - and
    /// confirms it parses to the expected id.
    ///
    /// This proves the mechanism on whatever object format the test host
    /// produces: ELF on the `ubuntu-latest` runner this workspace's own
    /// `.github/workflows/ci.yml` uses (the format `--gc-sections` actually
    /// drops sections on - a passing run there is the real regression
    /// guard), Mach-O on a macOS development machine (which never dropped
    /// the section even before this fix, so a local run confirms the
    /// section and its contents but does not by itself exercise the ELF
    /// regression - CI is what does).
    #[test]
    fn participant_metadata_section_survives_the_linker() -> Result<()> {
        let meta = linked_participant_metadata(
            "phoxal-elf-meta-probe",
            r#"use phoxal::prelude::*;

#[phoxal::service(id = "elf-meta-probe")]
struct ElfMetaProbe;

impl Participant for ElfMetaProbe {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<ElfMetaProbe>()
}
"#,
        )?;
        assert_eq!(
            meta.get("id").and_then(Value::as_str),
            Some("elf-meta-probe"),
            "unexpected participant metadata in the linked section: {meta:?}"
        );
        Ok(())
    }

    /// The root brain's complete process contract, proven on a real linked
    /// binary rather than on macro tokens: `#[phoxal::brain]` embeds exactly
    /// the fixed `brain` identity, the distinct `brain` kind, and the ordinary
    /// unit-config schema every `Config = ()` role emits - no more fields and
    /// no project-chosen identity. This is what `phoxal-cli` reads out of
    /// `bin/brain` before launching it.
    #[test]
    fn a_brain_binary_embeds_the_exact_root_brain_metadata_record() -> Result<()> {
        // The probe package is deliberately NOT named `brain`: the identity is
        // fixed by the role attribute, never derived from the package name.
        let meta = linked_participant_metadata(
            "some-robot-project",
            r#"use phoxal::prelude::*;

#[phoxal::brain]
struct Brain;

impl Participant for Brain {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Ok(((), ()))
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Brain>()
}
"#,
        )?;
        assert_eq!(
            meta,
            serde_json::json!({
                "schema": "phoxal/participant-metadata/v0",
                "id": "brain",
                "kind": "brain",
                "config_schema": {"type": "null"},
            }),
            "unexpected root brain metadata in the linked section"
        );
        Ok(())
    }

    /// Build `main_rs` as a standalone `package` binary against the in-tree
    /// `phoxal` crate and read its embedded participant-metadata section back
    /// out of the linked artifact. The binary is never executed.
    fn linked_participant_metadata(package: &str, main_rs: &str) -> Result<Value> {
        use object::{Object, ObjectSection};

        let workspace_root = workspace_root()?;
        let phoxal_path = workspace_root.join("phoxal");

        let probe_dir = tempfile::tempdir().context("failed to create temp probe crate dir")?;
        let crate_dir = probe_dir.path();
        fs::create_dir_all(crate_dir.join("src"))?;
        fs::write(
            crate_dir.join("Cargo.toml"),
            format!(
                r#"[workspace]

[package]
name = "{package}"
version = "0.1.0"
edition = "2024"
publish = false

[[bin]]
name = "{package}"
path = "src/main.rs"

[dependencies]
phoxal = {{ path = {phoxal_path:?} }}
"#,
                phoxal_path = phoxal_path.display().to_string(),
            ),
        )?;
        fs::write(crate_dir.join("src/main.rs"), main_rs)?;

        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let status = std::process::Command::new(&cargo)
            .arg("build")
            .arg("--manifest-path")
            .arg(crate_dir.join("Cargo.toml"))
            .status()
            .context("failed to spawn cargo build for the probe participant")?;
        assert!(
            status.success(),
            "cargo build for the linker-section probe participant failed"
        );

        let binary_name = if cfg!(windows) {
            format!("{package}.exe")
        } else {
            package.to_string()
        };
        let binary_path = crate_dir.join("target").join("debug").join(binary_name);
        let data = fs::read(&binary_path).with_context(|| {
            format!(
                "failed to read the built probe binary at {}",
                binary_path.display()
            )
        })?;
        let file = object::File::parse(&*data).with_context(|| {
            format!(
                "{} is not a recognized object file (ELF/Mach-O/...)",
                binary_path.display()
            )
        })?;

        let mut section_bytes = None;
        for name in PARTICIPANT_META_SECTION_NAMES {
            if let Some(section) = file.section_by_name(name) {
                section_bytes = Some(section.data().with_context(|| {
                    format!("failed to read section '{name}' data from the probe binary")
                })?);
                break;
            }
        }
        let section_bytes = section_bytes.with_context(|| {
            format!(
                "the built probe binary carries no participant metadata section ({}); the ELF \
                 --gc-sections defeat in phoxal-macros' expand_participant/Participant::\
                 __retain_embedded_metadata has regressed",
                PARTICIPANT_META_SECTION_NAMES.join(" or ")
            )
        })?;

        serde_json::from_slice(section_bytes)
            .context("the participant metadata section did not parse as JSON")
    }

    #[test]
    fn package_identity_is_provider_qualified() {
        assert_eq!(
            package_identity(ArtifactKind::Service, "drive"),
            "phoxal/service-drive"
        );
        assert_eq!(
            package_identity(ArtifactKind::Component, "ddsm115"),
            "phoxal/component-ddsm115"
        );
        assert_eq!(
            package_identity(ArtifactKind::Simulator, "webots-controller"),
            "phoxal/simulator-webots-controller"
        );
    }

    #[test]
    fn compiled_asset_id_rules_match_at_the_producer_and_consumer_boundary() {
        let cases = [
            ("meshes/base.stl", true),
            ("components/camera/config.json", true),
            ("", false),
            ("/absolute", false),
            ("../secret", false),
            ("a/../b", false),
            ("a\\b", false),
            ("a//b", false),
            ("a/./b", false),
        ];
        for (value, accepted) in cases {
            assert_eq!(
                phoxal_manifest::AssetId::new(value).is_ok(),
                accepted,
                "manifest producer disagreed for {value:?}"
            );
            assert_eq!(
                phoxal::AssetId::new(value).is_ok(),
                accepted,
                "runtime consumer disagreed for {value:?}"
            );
        }
    }
}
