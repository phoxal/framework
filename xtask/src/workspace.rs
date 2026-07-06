use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use cargo_metadata::{MetadataCommand, TargetKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const LIBRARY_CRATE_DIRS: [&str; 4] = ["phoxal", "phoxal-api", "phoxal-bus", "phoxal-macros"];
const EXCLUDED_TOP_LEVEL_DIRS: [&str; 2] = ["xtask", "fixture"];
const LINUX_TARGETS: [&str; 2] = ["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"];
const DARWIN_TARGET: &str = "aarch64-apple-darwin";

/// Official Phoxal packages always use this provider segment in their public
/// `package` identity (`phoxal/<name>`). Third-party catalogs will use their own
/// provider segment; the grammar has no other official provider today.
pub const PHOXAL_PROVIDER: &str = "phoxal";

/// The target-independent scope token recorded for a [`ArtifactKind::ComponentAssets`]
/// package instead of a real target triple: assets are not per-architecture
/// binaries, so `target_triples`/`release_assets`/`status` carry exactly this one
/// key rather than pretending to be a per-triple binary matrix.
pub const TARGET_INDEPENDENT_SCOPE: &str = "target-independent";

/// The file xtask reads for a driverless-package's component assets version,
/// since a `ComponentAssets` package has no `Cargo.toml` to carry a `[package]
/// version`. Lives beside `component.yaml` at the component root; holds nothing
/// but a trimmed semver string. This is xtask-owned release metadata, not part
/// of the `phoxal` robot/component model that parses `component.yaml`.
pub const ASSETS_VERSION_FILE: &str = "assets.version";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Service,
    /// Target-independent component files: `component.yaml`, `simulation.yaml`,
    /// `structure.urdf`, meshes, and other asset data. Discovered from
    /// `component/<id>/component.yaml`; carries no Cargo crate.
    ComponentAssets,
    /// The optional target-specific checked driver binary for a component.
    /// Discovered from `component/<id>/driver/Cargo.toml`.
    ComponentDriver,
    Tool,
    Simulator,
}

impl ArtifactKind {
    /// The `artifact.kind` string a real participant binary reports from its
    /// `emit-apis` output. This is the runtime's own vocabulary
    /// (`phoxal-macros`' `AuthoringKind::artifact_kind()`, `phoxal::check::ParticipantKind`)
    /// and is intentionally distinct from [`Self::catalog_kind`]: the runtime
    /// authoring surface still calls a component driver `"driver"` and does not
    /// know about xtask's catalog vocabulary. `ComponentAssets` has no runtime
    /// binary at all, so this value is never asserted against a real subprocess;
    /// it exists only for uniform display (e.g. `release discover` tables).
    pub fn emit_apis_kind(self) -> &'static str {
        match self {
            ArtifactKind::Service => "service",
            ArtifactKind::ComponentAssets => "component_assets",
            ArtifactKind::ComponentDriver => "driver",
            ArtifactKind::Tool => "tool",
            ArtifactKind::Simulator => "simulator",
        }
    }

    /// The catalog's serialized `kind` tag (`xtask/src/catalog/schema.rs`). Unlike
    /// [`Self::emit_apis_kind`], this is the public catalog vocabulary Phase 7
    /// introduces for the assets/driver split.
    pub fn catalog_kind(self) -> &'static str {
        match self {
            ArtifactKind::Service => "service",
            ArtifactKind::ComponentAssets => "component_assets",
            ArtifactKind::ComponentDriver => "component_driver",
            ArtifactKind::Tool => "tool",
            ArtifactKind::Simulator => "simulator",
        }
    }

    /// Whether this kind is a target-independent package: a single
    /// [`TARGET_INDEPENDENT_SCOPE`] token rather than a per-triple binary matrix.
    pub fn is_target_independent(self) -> bool {
        matches!(self, ArtifactKind::ComponentAssets)
    }

    /// Whether this kind has a Cargo crate backing it. `ComponentAssets` is
    /// discovered from `component.yaml` alone and has no `Cargo.toml`/binary.
    pub fn has_crate(self) -> bool {
        !matches!(self, ArtifactKind::ComponentAssets)
    }
}

impl fmt::Display for ArtifactKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.pad(self.catalog_kind())
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
}

#[derive(Clone, Debug, Serialize)]
pub struct OfficialArtifact {
    /// The provider-qualified public identity, e.g. `phoxal/component-ddsm115-driver`.
    /// This is the sole public identity; release tags/assets are filesystem-safe
    /// projections of it (see [`Self::release_tag`]).
    pub package: String,
    /// The Cargo crate name backing this package (e.g. `phoxal-component-ddsm115-driver`),
    /// or `None` for a [`ArtifactKind::ComponentAssets`] package, which has no
    /// `Cargo.toml`. Internal-only: never a public identity.
    pub package_name: Option<String>,
    pub kind: ArtifactKind,
    pub version: String,
    pub crate_dir: PathBuf,
    /// The release binary name, or `None` for a [`ArtifactKind::ComponentAssets`]
    /// package, which has no binary target.
    pub bin_name: Option<String>,
    pub id: String,
    pub metadata: PhoxalPackageMetadata,
}

impl OfficialArtifact {
    /// The release tag: a filesystem-safe projection of the provider-qualified
    /// `package` (`phoxal/component-ddsm115-driver` -> `phoxal-component-ddsm115-driver-v0.1.5`).
    pub fn release_tag(&self) -> String {
        format!(
            "{}-v{}",
            filesystem_safe_package(&self.package),
            self.version
        )
    }

    pub fn supported_target_triples(&self) -> Vec<String> {
        if self.kind.is_target_independent() {
            return vec![TARGET_INDEPENDENT_SCOPE.to_string()];
        }
        let mut targets = LINUX_TARGETS
            .iter()
            .map(|target| (*target).to_string())
            .collect::<Vec<_>>();
        if matches!(self.kind, ArtifactKind::Tool | ArtifactKind::Simulator) {
            targets.push(DARWIN_TARGET.to_string());
        }
        targets.extend(self.metadata.extra_target_triples.iter().cloned());
        targets.sort();
        targets.dedup();
        targets
    }

    pub fn supports_target(&self, target: &str) -> bool {
        self.supported_target_triples()
            .iter()
            .any(|candidate| candidate == target)
    }

    /// The Cargo crate name, or an error naming the package if it has none
    /// (a [`ArtifactKind::ComponentAssets`] package has no `Cargo.toml`/binary,
    /// so crate-oriented operations - build, package, upload, bump - do not apply).
    pub fn require_package_name(&self) -> Result<&str> {
        self.package_name.as_deref().with_context(|| {
            format!(
                "{} is a {} package with no Cargo crate; this operation only applies to \
                 crate-backed packages",
                self.package, self.kind
            )
        })
    }

    /// The release binary name, or an error naming the package if it has none.
    pub fn require_bin_name(&self) -> Result<&str> {
        self.bin_name.as_deref().with_context(|| {
            format!(
                "{} is a {} package with no binary target; this operation only applies to \
                 crate-backed packages",
                self.package, self.kind
            )
        })
    }
}

/// Projects a provider-qualified `package` (`phoxal/component-ddsm115-driver`)
/// to its filesystem-safe form (`phoxal-component-ddsm115-driver`) for release
/// tags and asset filenames (`docs #21` "Release tags and assets").
pub fn filesystem_safe_package(package: &str) -> String {
    package.replace('/', "-")
}

pub fn runner_for_target(target: &str) -> Result<&'static str> {
    match target {
        "aarch64-unknown-linux-gnu" => Ok("ubuntu-24.04-arm"),
        "x86_64-unknown-linux-gnu" => Ok("ubuntu-24.04"),
        "aarch64-apple-darwin" => Ok("macos-14"),
        // `ComponentAssets` packaging just tars files (no cargo build, no
        // per-architecture binary), so any host runner works; the cheapest
        // Linux runner is used.
        TARGET_INDEPENDENT_SCOPE => Ok("ubuntu-24.04"),
        _ => bail!("no CI runner is configured for release target {target}"),
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
        let metadata = MetadataCommand::new()
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

        official_artifacts.extend(discover_component_assets(&root)?);

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

    #[cfg(test)]
    pub(crate) fn from_parts_for_tests(
        root: PathBuf,
        target_dir: PathBuf,
        official_artifacts: Vec<OfficialArtifact>,
    ) -> Self {
        Self {
            root,
            target_dir,
            official_artifacts,
        }
    }
}

/// Discovers every `component/<id>/component.yaml` as a target-independent
/// `ComponentAssets` package. Unlike crate-backed artifacts, these are not seen
/// by `cargo_metadata` (no `Cargo.toml`), so they are walked directly off disk.
fn discover_component_assets(root: &Path) -> Result<Vec<OfficialArtifact>> {
    let component_root = root.join("component");
    if !component_root.is_dir() {
        return Ok(Vec::new());
    }

    let mut assets = Vec::new();
    let mut entries = fs::read_dir(&component_root)
        .with_context(|| format!("failed to read {}", component_root.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to read {}", component_root.display()))?;
    entries.sort();

    for component_dir in entries {
        if !component_dir.is_dir() {
            continue;
        }
        let component_yaml = component_dir.join("component.yaml");
        if !component_yaml.is_file() {
            continue;
        }
        let id = component_dir
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("{} has no UTF-8 directory name", component_dir.display()))?
            .to_string();
        if id.is_empty() {
            bail!("{} has an empty component id", component_dir.display());
        }
        let version = read_assets_version(&component_dir, &id)?;
        assets.push(OfficialArtifact {
            package: package_identity(ArtifactKind::ComponentAssets, &id),
            package_name: None,
            kind: ArtifactKind::ComponentAssets,
            version,
            crate_dir: component_dir,
            bin_name: None,
            id,
            metadata: PhoxalPackageMetadata::default(),
        });
    }

    Ok(assets)
}

fn read_assets_version(component_dir: &Path, id: &str) -> Result<String> {
    let version_path = component_dir.join(ASSETS_VERSION_FILE);
    let text = fs::read_to_string(&version_path).with_context(|| {
        format!(
            "component '{id}' has a component.yaml but no {} recording its assets package version",
            version_path.display()
        )
    })?;
    let version = text.trim();
    if version.is_empty() {
        bail!("{} is empty", version_path.display());
    }
    Ok(version.to_string())
}

/// The provider-qualified public identity for a discovered package
/// (`phoxal/service-drive`, `phoxal/component-ddsm115-assets`, ...).
pub fn package_identity(kind: ArtifactKind, id: &str) -> String {
    format!("{PHOXAL_PROVIDER}/{}", package_name_segment(kind, id))
}

fn package_name_segment(kind: ArtifactKind, id: &str) -> String {
    match kind {
        ArtifactKind::Service => format!("service-{id}"),
        ArtifactKind::ComponentAssets => format!("component-{id}-assets"),
        ArtifactKind::ComponentDriver => format!("component-{id}-driver"),
        ArtifactKind::Tool => format!("tool-{id}"),
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

    if top_level == "component" {
        if components.len() != 4
            || components[1].is_empty()
            || components[2] != "driver"
            || components[3] != "Cargo.toml"
        {
            bail!(
                "workspace package manifest {} is nested under artifact root 'component'; \
                 component driver crates must live exactly at \
                 component/<id>/driver/Cargo.toml",
                relative.display()
            );
        }
        return Ok(ManifestClassification::Artifact {
            kind: ArtifactKind::ComponentDriver,
            id: components[1].to_string(),
        });
    }

    let Some(kind) = artifact_kind_from_directory(top_level) else {
        return Ok(ManifestClassification::NonArtifact);
    };
    if components.len() != 3 || components[2] != "Cargo.toml" || components[1].is_empty() {
        bail!(
            "workspace package manifest {} is nested under artifact root '{}'; official artifacts \
             must live exactly at {{service,tool,simulator}}/<id>/Cargo.toml",
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
        "tool" => Some(ArtifactKind::Tool),
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

fn validate_artifact_publish(
    package_name: &str,
    publish: Option<&[String]>,
    root: &Path,
    manifest_path: &Path,
) -> Result<()> {
    if !publish.is_some_and(|registries| registries.is_empty()) {
        bail!(
            "{package_name} is an official artifact but {} does not set publish = false; \
             artifacts are git-released by xtask (`cargo xtask release cut`) and must never be \
             crates.io-publishable. Set publish = false",
            relative_display(root, manifest_path)
        );
    }
    Ok(())
}

/// The Cargo `package.name` a crate-backed artifact must use for its kind + id.
/// `ComponentAssets` has no Cargo crate, so it has no entry here; its identity
/// comes from [`package_identity`] alone.
fn expected_package_name(kind: ArtifactKind, id: &str) -> String {
    match kind {
        ArtifactKind::Service => format!("phoxal-service-{id}"),
        ArtifactKind::ComponentAssets => {
            unreachable!("ComponentAssets has no Cargo crate to validate a package name against")
        }
        ArtifactKind::ComponentDriver => format!("phoxal-component-{id}-driver"),
        ArtifactKind::Tool => format!("phoxal-tool-{id}"),
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
                .and_then(|rest| rest.strip_suffix("-driver"))
                .map(|id| (ArtifactKind::ComponentDriver, id.to_string()))
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
    use serde_json::json;

    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/repo")
    }

    fn classify(relative: &str) -> Result<ManifestClassification> {
        classify_manifest_path(&root(), &root().join(relative))
    }

    #[test]
    fn artifact_publish_true_is_an_error() {
        let manifest = root().join("simulator/webots/Cargo.toml");

        validate_artifact_publish("phoxal-simulator-webots", Some(&[]), &root(), &manifest)
            .expect("publish = false is valid: xtask git-releases artifacts");

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
            classify("component/ddsm115/driver/Cargo.toml")?,
            ManifestClassification::Artifact {
                kind: ArtifactKind::ComponentDriver,
                id: "ddsm115".to_string()
            }
        );
        assert_eq!(
            classify("tool/router/Cargo.toml")?,
            ManifestClassification::Artifact {
                kind: ArtifactKind::Tool,
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
    fn component_crate_directly_under_component_id_is_an_error() {
        // Pre-Phase-7 layout (`component/<id>/Cargo.toml`) is no longer valid: the
        // driver crate must live one level deeper, at `component/<id>/driver/`.
        let err = classify("component/ddsm115/Cargo.toml").unwrap_err();
        assert!(
            err.to_string().contains("component/<id>/driver/Cargo.toml"),
            "{err}"
        );
    }

    #[test]
    fn nested_crates_under_artifact_roots_are_errors() {
        let err = classify("service/drive/helper/Cargo.toml").unwrap_err();
        assert!(err.to_string().contains("nested under artifact root"));
    }

    #[test]
    fn nested_crates_under_component_driver_are_errors() {
        let err = classify("component/ddsm115/driver/helper/Cargo.toml").unwrap_err();
        assert!(err.to_string().contains("component/<id>/driver/Cargo.toml"));
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
    fn package_identity_is_provider_qualified() {
        assert_eq!(
            package_identity(ArtifactKind::Service, "drive"),
            "phoxal/service-drive"
        );
        assert_eq!(
            package_identity(ArtifactKind::ComponentAssets, "ddsm115"),
            "phoxal/component-ddsm115-assets"
        );
        assert_eq!(
            package_identity(ArtifactKind::ComponentDriver, "ddsm115"),
            "phoxal/component-ddsm115-driver"
        );
        assert_eq!(
            package_identity(ArtifactKind::Tool, "router"),
            "phoxal/tool-router"
        );
        assert_eq!(
            package_identity(ArtifactKind::Simulator, "webots-supervisor"),
            "phoxal/simulator-webots-supervisor"
        );
    }

    #[test]
    fn release_tag_projects_provider_qualified_package_to_filesystem_safe_form() {
        let artifact = OfficialArtifact {
            package: "phoxal/component-ddsm115-driver".to_string(),
            package_name: Some("phoxal-component-ddsm115-driver".to_string()),
            kind: ArtifactKind::ComponentDriver,
            version: "0.1.5".to_string(),
            crate_dir: PathBuf::from("component/ddsm115/driver"),
            bin_name: Some("phoxal-component-ddsm115-driver".to_string()),
            id: "ddsm115".to_string(),
            metadata: PhoxalPackageMetadata::default(),
        };
        assert_eq!(
            artifact.release_tag(),
            "phoxal-component-ddsm115-driver-v0.1.5"
        );
    }

    #[test]
    fn component_assets_supports_only_the_target_independent_scope() {
        let artifact = OfficialArtifact {
            package: "phoxal/component-ddsm115-assets".to_string(),
            package_name: None,
            kind: ArtifactKind::ComponentAssets,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::from("component/ddsm115"),
            bin_name: None,
            id: "ddsm115".to_string(),
            metadata: PhoxalPackageMetadata::default(),
        };
        assert_eq!(
            artifact.supported_target_triples(),
            vec![TARGET_INDEPENDENT_SCOPE.to_string()]
        );
        assert!(artifact.supports_target(TARGET_INDEPENDENT_SCOPE));
        assert!(!artifact.supports_target("x86_64-unknown-linux-gnu"));
        assert!(artifact.require_package_name().is_err());
        assert!(artifact.require_bin_name().is_err());
    }
}
