use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use cargo_metadata::{MetadataCommand, TargetKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const LIBRARY_CRATE_DIRS: [&str; 4] = ["phoxal", "phoxal-api", "phoxal-bus", "phoxal-macros"];
const EXCLUDED_TOP_LEVEL_DIRS: [&str; 2] = ["xtask", "fixture"];

/// The uniform release matrix every binary artifact builds for (design doc
/// `organization/tmp/ci-release-refactor/design.md` §3/§4.1): no per-kind
/// target selection - a `Tool` and a `Service` build for exactly the same
/// triples. `x86_64-apple-darwin` is intentionally absent: GitHub is retiring
/// its last Intel macOS image (`macos-13`), and it left release build waves
/// queued indefinitely for a runner that never arrives. Nearly all dev hosts
/// are Apple Silicon now; an Intel macOS target can be re-added later (as an
/// ARM-host cross-build) if a user actually needs it.
const BINARY_TARGETS: [&str; 5] = [
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
];

/// Official Phoxal packages always use this provider segment in their public
/// `package` identity (`phoxal/<name>`). Third-party suites use their own
/// provider segment; the grammar has no other official provider today.
pub const PHOXAL_PROVIDER: &str = "phoxal";

/// The assets scope token recorded for a [`ArtifactKind::Component`] package's
/// asset bundle instead of a real target triple. It is purely an internal
/// packaging scope sentinel - the suite schema itself stores this output as
/// `assets: Option<Blob>` with no such key - and [`crate::release::suite`]'s
/// generator maps a target equal to this sentinel into that `assets` field
/// rather than into the per-triple `targets` map.
pub const ASSETS_SCOPE: &str = "assets";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Service,
    /// A component crate: ships the driver binary (built for
    /// [`ASSETS_SCOPE`]'s sibling per-target triples) AND the component's asset
    /// bundle (`component.yaml`,
    /// `simulation.yaml`, `structure.urdf`, `meshes/` if present) in one
    /// crate/release (design doc §9). Discovered from
    /// `component/<id>/Cargo.toml`.
    Component,
    Tool,
    Simulator,
    /// Phoxal-owned process infrastructure. It is released like other binary
    /// artifacts but is not a participant and must not embed API metadata.
    Infrastructure,
}

impl ArtifactKind {
    /// The suite's serialized artifact `kind` tag (`xtask/src/suite.rs`).
    pub fn suite_kind(self) -> &'static str {
        match self {
            ArtifactKind::Service => "service",
            ArtifactKind::Component => "component",
            ArtifactKind::Tool => "tool",
            ArtifactKind::Simulator => "simulator",
            ArtifactKind::Infrastructure => "infrastructure",
        }
    }

    /// Whether this kind ships an asset bundle in addition to its per-target
    /// binary output (design doc §9: every `component/` crate carries both).
    pub fn ships_assets(self) -> bool {
        matches!(self, ArtifactKind::Component)
    }

    /// Whether binaries of this kind participate in the API coherence graph.
    pub fn embeds_participant_metadata(self) -> bool {
        !matches!(self, ArtifactKind::Infrastructure)
    }
}

impl fmt::Display for ArtifactKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.pad(self.suite_kind())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PhoxalPackageMetadata {
    /// Extra release target triples for genuine per-crate exceptions. The normal
    /// target matrix is derived from the artifact kind, not copied into manifests.
    #[serde(
        default,
        rename = "extra-target-triples",
        alias = "extra_target_triples"
    )]
    pub extra_target_triples: Vec<String>,
    /// Target triples this artifact cannot build for. Entries are glob
    /// patterns, so a package can exclude a target family such as `*-musl` or
    /// name an exact triple.
    #[serde(default, rename = "unsupported-targets")]
    pub unsupported_targets: Vec<String>,
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
    /// crate (design doc §9: a crate with an empty `src/lib.rs` and no
    /// `[[bin]]`, ships only its asset bundle). None of today's discovered
    /// components are driver-less.
    pub bin_name: Option<String>,
    pub id: String,
    pub metadata: PhoxalPackageMetadata,
}

impl OfficialArtifact {
    /// The full set of release targets this artifact packages for: its
    /// per-triple binary targets, plus [`ASSETS_SCOPE`] when the kind also
    /// ships an asset bundle
    /// ([`ArtifactKind::ships_assets`], design doc §9).
    pub fn supported_target_triples(&self) -> Vec<String> {
        // Every discovered artifact starts from the uniform five-target matrix
        // (#197); a `Component` additionally ships the asset bundle (design
        // doc §9). Package metadata may add exceptional
        // targets and subtract targets the artifact cannot build.
        let mut targets = BINARY_TARGETS
            .iter()
            .map(|target| (*target).to_string())
            .collect::<Vec<_>>();
        if self.kind.ships_assets() {
            targets.push(ASSETS_SCOPE.to_string());
        }
        targets.extend(self.metadata.extra_target_triples.iter().cloned());
        targets.retain(|target| {
            !self.metadata.unsupported_targets.iter().any(|pattern| {
                glob::Pattern::new(pattern)
                    .expect("unsupported-targets patterns are validated during discovery")
                    .matches(target)
            })
        });
        targets.sort();
        targets.dedup();
        targets
    }

    pub fn supports_target(&self, target: &str) -> bool {
        self.supported_target_triples()
            .iter()
            .any(|candidate| candidate == target)
    }

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

    /// The release binary name, or an error naming the package if it has none
    /// (a driver-less component crate - see [`Self::bin_name`]).
    pub fn require_bin_name(&self) -> Result<&str> {
        self.bin_name.as_deref().with_context(|| {
            format!(
                "{} is a {} package with no binary target; this operation only applies to \
                 packages with a driver binary",
                self.package, self.kind
            )
        })
    }
}

/// Projects a provider-qualified `package` (`phoxal/component-ddsm115`)
/// to its filesystem-safe form (`phoxal-component-ddsm115`) for asset
/// filenames.
pub fn filesystem_safe_package(package: &str) -> String {
    package.replace('/', "-")
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
            let phoxal_metadata = parse_phoxal_metadata(&package_name, &package.metadata)?;
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
                .map(|target| target.name.clone())
                .with_context(|| {
                    format!(
                        "{package_name} is an official artifact package but has no binary target"
                    )
                })?;

            official_artifacts.push(OfficialArtifact {
                package: package_identity(kind, &id),
                package_name: Some(package_name),
                kind,
                version: package.version.to_string(),
                crate_dir,
                bin_name: Some(bin_name),
                id,
                metadata: phoxal_metadata,
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

    pub fn official_artifact(&self, package: &str) -> Result<&OfficialArtifact> {
        self.official_artifacts
            .iter()
            .find(|artifact| {
                artifact.package == package || artifact.package_name.as_deref() == Some(package)
            })
            .with_context(|| {
                let known = self
                    .official_artifacts
                    .iter()
                    .map(|artifact| artifact.package.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("unknown official artifact package {package}; known packages: {known}")
            })
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

fn validate_artifact_publish(
    package_name: &str,
    publish: Option<&[String]>,
    root: &Path,
    manifest_path: &Path,
) -> Result<()> {
    if !publish.is_some_and(|registries| registries.is_empty()) {
        bail!(
            "{package_name} is an official artifact but {} does not set publish = false; \
             artifacts stay outside release-plz and crates.io - the release workflow builds \
             and packages them with xtask and stages them into the GitHub train release. \
             Set publish = false",
            relative_display(root, manifest_path)
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

fn parse_phoxal_metadata(package_name: &str, metadata: &Value) -> Result<PhoxalPackageMetadata> {
    let Some(phoxal_metadata) = metadata.get("phoxal") else {
        return Ok(PhoxalPackageMetadata::default());
    };
    if phoxal_metadata.get("kind").is_some() || phoxal_metadata.get("id").is_some() {
        bail!(
            "{package_name} [package.metadata.phoxal] must not declare kind or id; those are \
             derived from the directory convention"
        );
    }

    let mut parsed: PhoxalPackageMetadata = serde_json::from_value(phoxal_metadata.clone())
        .with_context(|| format!("{package_name} has invalid [package.metadata.phoxal]"))?;
    let mut seen = BTreeSet::new();
    for triple in &parsed.extra_target_triples {
        if triple.trim().is_empty() {
            bail!(
                "{package_name} [package.metadata.phoxal] extra-target-triples contains an empty triple"
            );
        }
        if !seen.insert(triple.clone()) {
            bail!(
                "{package_name} [package.metadata.phoxal] extra-target-triples contains duplicate {triple}"
            );
        }
    }
    parsed.extra_target_triples.sort();
    seen.clear();
    for pattern in &parsed.unsupported_targets {
        if pattern.trim().is_empty() {
            bail!(
                "{package_name} [package.metadata.phoxal] unsupported-targets contains an empty pattern"
            );
        }
        glob::Pattern::new(pattern).with_context(|| {
            format!(
                "{package_name} [package.metadata.phoxal] unsupported-targets contains invalid glob {pattern}"
            )
        })?;
        if !seen.insert(pattern.clone()) {
            bail!(
                "{package_name} [package.metadata.phoxal] unsupported-targets contains duplicate {pattern}"
            );
        }
    }
    parsed.unsupported_targets.sort();
    Ok(parsed)
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

pub fn require_nonempty_artifacts(artifacts: &[OfficialArtifact]) -> Result<()> {
    if artifacts.is_empty() {
        bail!("no official release artifacts were discovered");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/repo")
    }

    fn classify(relative: &str) -> Result<ManifestClassification> {
        classify_manifest_path(&root(), &root().join(relative))
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
    fn official_tools_do_not_import_logical_clock_surfaces() -> Result<()> {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .context("xtask manifest directory has no workspace parent")?;
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
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .context("xtask manifest directory has no workspace parent")?;
        for tool in ["joypad", "bus", "device"] {
            let source = workspace_root.join("tool").join(tool).join("src/main.rs");
            let body = fs::read_to_string(&source)?;
            assert!(
                body.contains("set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip)"),
                "periodic tool {tool} must skip missed ticks instead of catching up"
            );
        }
        Ok(())
    }

    #[test]
    fn zenoh_dependency_profiles_keep_transport_compression_disabled() -> Result<()> {
        let workspace_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .context("xtask manifest directory has no workspace parent")?
            .join("Cargo.toml");
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
            direct_zenoh_dependencies, 4,
            "every direct Zenoh dependency must be covered by this guard"
        );
        Ok(())
    }

    #[test]
    fn real_workspace_release_scope_is_valid() -> Result<()> {
        let workspace_manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .context("xtask manifest directory has no workspace parent")?
            .join("Cargo.toml");
        let workspace =
            Workspace::discover_with(MetadataCommand::new().manifest_path(workspace_manifest))?;

        assert_eq!(workspace.official_artifacts().len(), 27);

        assert_eq!(
            workspace
                .official_artifacts()
                .iter()
                .filter(|artifact| artifact.kind == ArtifactKind::Service)
                .count(),
            14
        );
        Ok(())
    }

    #[test]
    fn artifact_publish_true_is_an_error() {
        let manifest = root().join("simulator/webots/Cargo.toml");

        validate_artifact_publish("phoxal-simulator-webots", Some(&[]), &root(), &manifest).expect(
            "publish = false is valid: the release workflow builds and packages artifacts \
             with xtask and stages them into the train release",
        );

        let error = validate_artifact_publish("phoxal-simulator-webots", None, &root(), &manifest)
            .unwrap_err();
        assert!(error.to_string().contains("publish = false"), "{error}");

        let error = validate_artifact_publish(
            "phoxal-simulator-webots",
            Some(&["crates-io".to_string()]),
            &root(),
            &manifest,
        )
        .unwrap_err();
        assert!(error.to_string().contains("publish = false"), "{error}");
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
    fn library_xtask_and_fixture_paths_are_excluded() -> Result<()> {
        assert_eq!(
            classify("phoxal/Cargo.toml")?,
            ManifestClassification::Excluded
        );
        assert_eq!(
            classify("xtask/Cargo.toml")?,
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
    fn metadata_phoxal_rejects_kind_and_id() {
        let err = parse_phoxal_metadata(
            "phoxal-service-drive",
            &json!({ "phoxal": { "kind": "service" } }),
        )
        .unwrap_err();
        assert!(err.to_string().contains("must not declare kind or id"));
    }

    #[test]
    fn metadata_phoxal_accepts_extra_target_triples() -> Result<()> {
        let metadata = parse_phoxal_metadata(
            "phoxal-service-drive",
            &json!({
                "phoxal": {
                    "extra-target-triples": [
                        "x86_64-apple-darwin",
                        "aarch64-apple-darwin"
                    ]
                }
            }),
        )?;
        assert_eq!(
            metadata.extra_target_triples,
            vec!["aarch64-apple-darwin", "x86_64-apple-darwin"]
        );
        Ok(())
    }

    #[test]
    fn artifact_target_exclusions_support_globs_and_exact_triples() -> Result<()> {
        let metadata = parse_phoxal_metadata(
            "phoxal-tool-joypad",
            &json!({
                "phoxal": {
                    "extra-target-triples": ["riscv64gc-unknown-linux-gnu"],
                    "unsupported-targets": ["*-musl", "aarch64-apple-darwin"]
                }
            }),
        )?;
        let artifact = OfficialArtifact {
            package: "phoxal/tool-joypad".to_string(),
            package_name: Some("phoxal-tool-joypad".to_string()),
            kind: ArtifactKind::Tool,
            version: "0.1.6".to_string(),
            crate_dir: PathBuf::from("tool/joypad"),
            bin_name: Some("phoxal-tool-joypad".to_string()),
            id: "joypad".to_string(),
            metadata,
        };

        assert_eq!(
            artifact.supported_target_triples(),
            vec![
                "aarch64-unknown-linux-gnu",
                "riscv64gc-unknown-linux-gnu",
                "x86_64-unknown-linux-gnu",
            ]
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
            package_identity(ArtifactKind::Simulator, "webots-supervisor"),
            "phoxal/simulator-webots-supervisor"
        );
        assert_eq!(
            package_identity(ArtifactKind::Infrastructure, "router"),
            "phoxal/infrastructure-router"
        );
    }

    #[test]
    fn component_supports_its_binary_targets_and_the_assets_scope() {
        let artifact = OfficialArtifact {
            package: "phoxal/component-ddsm115".to_string(),
            package_name: Some("phoxal-component-ddsm115".to_string()),
            kind: ArtifactKind::Component,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::from("component/ddsm115"),
            bin_name: Some("phoxal-component-ddsm115".to_string()),
            id: "ddsm115".to_string(),
            metadata: PhoxalPackageMetadata::default(),
        };
        assert_eq!(
            artifact.supported_target_triples(),
            vec![
                "aarch64-apple-darwin".to_string(),
                "aarch64-unknown-linux-gnu".to_string(),
                "aarch64-unknown-linux-musl".to_string(),
                ASSETS_SCOPE.to_string(),
                "x86_64-unknown-linux-gnu".to_string(),
                "x86_64-unknown-linux-musl".to_string(),
            ]
        );
        assert!(artifact.supports_target(ASSETS_SCOPE));
        assert!(artifact.supports_target("x86_64-unknown-linux-gnu"));
        // A component builds the darwin (Apple Silicon) and musl triples too.
        assert!(artifact.supports_target("aarch64-apple-darwin"));
    }
}
