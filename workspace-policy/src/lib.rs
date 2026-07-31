//! Workspace policy: the rules the framework workspace must obey as a whole,
//! and the tests that enforce them.
//!
//! No single crate owns these facts - that a package's directory, name and
//! `publish` field agree, that official tools stay off the logical clock, that
//! the zenoh dependency set keeps transport compression disabled - so they live
//! here, in a crate whose only purpose is to be a test target under
//! `cargo test --workspace`.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
#[cfg(test)]
use cargo_metadata::DependencyKind;
use cargo_metadata::{MetadataCommand, TargetKind};
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
    /// A component crate: the driver binary plus the component's assets
    /// (`component.yaml`, `simulation.yaml`, `structure.urdf`, and `meshes/`
    /// if present) in one package. `cargo package` picks the assets up by
    /// default, so they need no inclusion rules. Discovered from
    /// `component/<id>/Cargo.toml`.
    Component,
    Tool,
    Simulator,
    /// Phoxal-owned process infrastructure. It is released like other binary
    /// artifacts but is not a participant and must not embed participant metadata.
    Infrastructure,
}

impl fmt::Display for ArtifactKind {
    /// Renders the kind as its top-level directory segment, the same token
    /// `artifact_kind_from_directory` parses.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.pad(match self {
            ArtifactKind::Service => "service",
            ArtifactKind::Component => "component",
            ArtifactKind::Tool => "tool",
            ArtifactKind::Simulator => "simulator",
            ArtifactKind::Infrastructure => "infrastructure",
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
    /// Every discovered [`OfficialArtifact`] is crate-backed today; this stays
    /// `Option` for a future crate with no binary target (see
    /// [`Self::require_package_name`]).
    pub package_name: Option<String>,
    pub kind: ArtifactKind,
    pub version: String,
    pub crate_dir: PathBuf,
    /// The release binary name, or `None` for a future driver-less component
    /// crate (design doc §9: a crate with no `[[bin]]` ships only its asset
    /// bundle). None of today's discovered
    /// components are driver-less.
    pub bin_name: Option<String>,
    pub id: String,
}

impl OfficialArtifact {
    /// The Cargo crate name, or an error naming the package if it has none.
    pub fn require_package_name(&self) -> Result<&str> {
        self.package_name.as_deref().with_context(|| {
            format!(
                "{} is a {} package with no Cargo crate; this operation only applies to \
                 crate-backed packages",
                self.package, self.kind
            )
        })
    }
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
            let bin_name = package
                .targets
                .iter()
                .filter(|target| target.is_kind(TargetKind::Bin))
                .find(|target| target.name == package_name)
                .or_else(|| {
                    package
                        .targets
                        .iter()
                        .find(|target| target.is_kind(TargetKind::Bin))
                })
                .map(|target| target.name.clone());
            if bin_name.is_none() && kind != ArtifactKind::Component {
                bail!("{package_name} is an official artifact package but has no binary target");
            }

            official_artifacts.push(OfficialArtifact {
                package: package_identity(kind, &id),
                package_name: Some(package_name),
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

fn package_name_segment(kind: ArtifactKind, id: &str) -> String {
    match kind {
        ArtifactKind::Service => format!("service-{id}"),
        ArtifactKind::Component => format!("component-{id}"),
        ArtifactKind::Tool => format!("tool-{id}"),
        ArtifactKind::Simulator => format!("simulator-{id}"),
        ArtifactKind::Infrastructure => format!("infrastructure-{id}"),
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
             must live exactly at {{service,component,tool,simulator,infrastructure}}/<id>/Cargo.toml",
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
        "tool" => Some(ArtifactKind::Tool),
        "simulator" => Some(ArtifactKind::Simulator),
        "infrastructure" => Some(ArtifactKind::Infrastructure),
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
        ArtifactKind::Tool => format!("phoxal-tool-{id}"),
        ArtifactKind::Simulator => format!("phoxal-simulator-{id}"),
        ArtifactKind::Infrastructure => format!("phoxal-infrastructure-{id}"),
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
                .strip_prefix("phoxal-tool-")
                .map(|id| (ArtifactKind::Tool, id.to_string()))
        })
        .or_else(|| {
            package_name
                .strip_prefix("phoxal-simulator-")
                .map(|id| (ArtifactKind::Simulator, id.to_string()))
        })
        .or_else(|| {
            package_name
                .strip_prefix("phoxal-infrastructure-")
                .map(|id| (ArtifactKind::Infrastructure, id.to_string()))
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

    fn visit_rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(directory)
            .with_context(|| format!("failed to read {}", directory.display()))?
        {
            let path = entry?.path();
            if path.is_dir() {
                visit_rust_sources(&path, sources)?;
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                sources.push(path);
            }
        }
        Ok(())
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
        let mut joypad = None;
        for package in metadata.workspace_packages() {
            let path = package.manifest_path.clone().into_std_path_buf();
            let source = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let requirements = phoxal_manifest::build_requirements::requirements_from_manifest(
                &source,
                &path.display().to_string(),
            )?;
            declared_packages.extend(requirements.apt.iter().cloned());
            if package.name.as_str() == "phoxal-tool-joypad" {
                joypad = Some(requirements);
            }
        }

        let joypad = joypad.context("workspace has no phoxal-tool-joypad package")?;
        assert_eq!(
            joypad.apt.into_iter().collect::<Vec<_>>(),
            ["libudev-dev", "pkg-config"]
        );

        let declared = declared_packages.into_iter().collect::<Vec<_>>();
        let ci = fs::read_to_string(workspace_root.join(".github/workflows/ci.yml"))?;
        let ci_packages = ci
            .lines()
            .find_map(|line| line.trim().strip_prefix("system-packages:"))
            .context(".github/workflows/ci.yml declares no system-packages input")?
            .split_whitespace()
            .collect::<Vec<_>>();
        assert_eq!(
            ci_packages, declared,
            "CI system-packages must equal the manifest-declared union"
        );
        let release = fs::read_to_string(workspace_root.join(".github/workflows/release-plz.yml"))?;
        let release_packages = release
            .lines()
            .find_map(|line| line.trim().strip_prefix("sudo apt-get install -y"))
            .context(".github/workflows/release-plz.yml installs no packaging dependencies")?
            .split_whitespace()
            .collect::<Vec<_>>();
        assert_eq!(
            release_packages, declared,
            "release packaging dependencies must equal the manifest-declared union"
        );
        Ok(())
    }

    #[test]
    fn official_tools_do_not_import_logical_clock_surfaces() -> Result<()> {
        let workspace_root = workspace_root()?;
        let mut sources = Vec::new();
        visit_rust_sources(&workspace_root.join("tool"), &mut sources)?;
        let forbidden = [
            "ClockMode",
            "ClockSource",
            "SimulationClock",
            "StepContext",
            "simulation().clock()",
            ".clock()",
        ];

        let mut violations = Vec::new();
        for source in sources {
            let body = fs::read_to_string(&source)?;
            for token in forbidden {
                if body.contains(token) {
                    violations.push(format!("{} imports or calls {token}", source.display()));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "tools must remain host/event driven:\n{}",
            violations.join("\n")
        );
        Ok(())
    }

    #[test]
    fn official_periodic_tools_skip_missed_ticks() -> Result<()> {
        let workspace_root = workspace_root()?;
        for tool in ["joypad", "bus", "device"] {
            let source = workspace_root
                .join("tool")
                .join(tool)
                .join("src")
                .join(format!("{tool}.rs"));
            let body = fs::read_to_string(&source)?;
            assert!(
                body.contains("set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip)"),
                "periodic tool {tool} must skip missed ticks instead of catching up"
            );
        }
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
            ("phoxal", &["phoxal-manifest"]),
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
                (Some("service" | "component" | "simulator" | "tool"), _)
                    | (Some("infrastructure"), Some("router"))
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
            direct_zenoh_dependencies, 3,
            "every direct Zenoh dependency must be covered by this guard"
        );
        Ok(())
    }

    #[test]
    fn real_workspace_release_scope_is_valid() -> Result<()> {
        let workspace_manifest = workspace_root()?.join("Cargo.toml");
        let workspace =
            Workspace::discover_with(MetadataCommand::new().manifest_path(workspace_manifest))?;

        assert_eq!(workspace.official_artifacts().len(), 26);

        assert_eq!(
            workspace
                .official_artifacts()
                .iter()
                .filter(|artifact| artifact.kind == ArtifactKind::Service)
                .count(),
            14
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
            classify("tool/bus/Cargo.toml")?,
            ManifestClassification::Artifact {
                kind: ArtifactKind::Tool,
                id: "bus".to_string()
            }
        );
        assert_eq!(
            classify("infrastructure/router/Cargo.toml")?,
            ManifestClassification::Artifact {
                kind: ArtifactKind::Infrastructure,
                id: "router".to_string()
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

    /// Design doc §9: a driver-less component crate that ships only its asset
    /// bundle (`component.yaml`, `simulation.yaml`, `structure.urdf`) must
    /// still pass discovery even though it has no `[[bin]]` target - that is
    /// the whole point of giving it a library target instead. This builds a
    /// standalone throwaway workspace on disk (its own `[workspace]` table,
    /// so it is not swallowed as a member of this repo's workspace) with one
    /// bin-less official component package, and asserts discovery accepts it
    /// with `bin_name: None`.
    #[test]
    fn discovery_accepts_a_binless_official_component_package() -> Result<()> {
        let workspace_dir = tempfile::tempdir().context("failed to create temp workspace dir")?;
        let root = workspace_dir.path();

        fs::write(
            root.join("Cargo.toml"),
            r#"[workspace]
resolver = "3"
members = ["component/asset-only"]
"#,
        )?;

        let package_dir = root.join("component/asset-only");
        fs::create_dir_all(package_dir.join("src"))?;
        fs::write(
            package_dir.join("Cargo.toml"),
            r#"[package]
name = "phoxal-component-asset-only"
version = "0.1.0"
edition = "2024"
license = "AGPL-3.0-only"
publish = ["phoxal"]
description = "Driver-less official component: assets only."

[lib]
path = "src/lib.rs"
"#,
        )?;
        fs::write(
            package_dir.join("src/lib.rs"),
            "//! Cargo train anchor; this component ships no driver binary.\n",
        )?;
        fs::write(package_dir.join("component.yaml"), "id: asset-only\n")?;

        let workspace = Workspace::discover_with(
            MetadataCommand::new().manifest_path(root.join("Cargo.toml")),
        )?;

        assert_eq!(workspace.official_artifacts().len(), 1);
        let artifact = &workspace.official_artifacts()[0];
        assert_eq!(artifact.package, "phoxal/component-asset-only");
        assert_eq!(artifact.kind, ArtifactKind::Component);
        assert_eq!(
            artifact.package_name.as_deref(),
            Some("phoxal-component-asset-only")
        );
        assert_eq!(artifact.bin_name, None);
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
    /// this test does: it compiles a throwaway tool participant against the
    /// real in-tree `phoxal`/`phoxal-macros` crates in a standalone temp
    /// workspace (the same "spin up a disposable cargo project" pattern
    /// [`discovery_accepts_a_binless_official_component_package`] already
    /// uses), then reads the linked binary's metadata section back with the
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
name = "phoxal-elf-meta-probe"
version = "0.1.0"
edition = "2024"
publish = false

[[bin]]
name = "phoxal-elf-meta-probe"
path = "src/main.rs"

[dependencies]
phoxal = {{ path = {phoxal_path:?} }}
"#,
                phoxal_path = phoxal_path.display().to_string(),
            ),
        )?;
        fs::write(
            crate_dir.join("src/main.rs"),
            r#"use phoxal::prelude::*;

#[phoxal::tool(id = "elf-meta-probe")]
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
            "phoxal-elf-meta-probe.exe"
        } else {
            "phoxal-elf-meta-probe"
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

        let meta: Value = serde_json::from_slice(section_bytes)
            .context("the participant metadata section did not parse as JSON")?;
        assert_eq!(
            meta.get("id").and_then(Value::as_str),
            Some("elf-meta-probe"),
            "unexpected participant metadata in the linked section: {meta:?}"
        );
        Ok(())
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
            package_identity(ArtifactKind::Tool, "bus"),
            "phoxal/tool-bus"
        );
        assert_eq!(
            package_identity(ArtifactKind::Simulator, "webots-controller"),
            "phoxal/simulator-webots-controller"
        );
        assert_eq!(
            package_identity(ArtifactKind::Infrastructure, "router"),
            "phoxal/infrastructure-router"
        );
    }

    #[test]
    fn compiled_asset_id_rules_match_at_the_producer_and_consumer_boundary() {
        let cases = [
            ("behavior/catalog.json", true),
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
