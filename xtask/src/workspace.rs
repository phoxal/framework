use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use cargo_metadata::{MetadataCommand, TargetKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const LIBRARY_CRATE_DIRS: [&str; 4] = ["phoxal", "phoxal-api", "phoxal-bus", "phoxal-macros"];
const EXCLUDED_TOP_LEVEL_DIRS: [&str; 2] = ["xtask", "fixture"];
const LINUX_TARGETS: [&str; 2] = ["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"];
const DARWIN_TARGET: &str = "aarch64-apple-darwin";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Service,
    Driver,
    Tool,
    Simulator,
}

impl ArtifactKind {
    pub fn emit_apis_kind(self) -> &'static str {
        match self {
            ArtifactKind::Service => "service",
            ArtifactKind::Driver => "driver",
            ArtifactKind::Tool => "tool",
            ArtifactKind::Simulator => "simulator",
        }
    }
}

impl fmt::Display for ArtifactKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.pad(self.emit_apis_kind())
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
    pub package_name: String,
    pub kind: ArtifactKind,
    pub version: String,
    pub crate_dir: PathBuf,
    pub bin_name: String,
    pub id: String,
    pub metadata: PhoxalPackageMetadata,
}

impl OfficialArtifact {
    pub fn release_tag(&self) -> String {
        format!("{}-v{}", self.package_name, self.version)
    }

    pub fn supported_target_triples(&self) -> Vec<String> {
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
}

pub fn runner_for_target(target: &str) -> Result<&'static str> {
    match target {
        "aarch64-unknown-linux-gnu" => Ok("ubuntu-24.04-arm"),
        "x86_64-unknown-linux-gnu" => Ok("ubuntu-24.04"),
        "aarch64-apple-darwin" => Ok("macos-14"),
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
                         {{service,component,tool,simulator}}/<id>/Cargo.toml directory grammar",
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
                package_name,
                kind,
                version: package.version.to_string(),
                crate_dir,
                bin_name,
                id,
                metadata: phoxal_metadata,
            });
        }

        official_artifacts.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.package_name.cmp(&right.package_name))
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

    pub fn official_artifact(&self, package_name: &str) -> Result<&OfficialArtifact> {
        self.official_artifacts
            .iter()
            .find(|artifact| artifact.package_name == package_name)
            .with_context(|| {
                let known = self
                    .official_artifacts
                    .iter()
                    .map(|artifact| artifact.package_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("unknown official artifact package {package_name}; known packages: {known}")
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
             must live exactly at {{service,component,tool,simulator}}/<id>/Cargo.toml",
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
        "component" => Some(ArtifactKind::Driver),
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
            "{} is in artifact directory {} but package.name is '{}'; expected '{}'",
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

fn expected_package_name(kind: ArtifactKind, id: &str) -> String {
    match kind {
        ArtifactKind::Service => format!("phoxal-service-{id}"),
        ArtifactKind::Driver => format!("phoxal-driver-{id}"),
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
                .strip_prefix("phoxal-driver-")
                .map(|id| (ArtifactKind::Driver, id.to_string()))
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
            classify("component/ddsm115/Cargo.toml")?,
            ManifestClassification::Artifact {
                kind: ArtifactKind::Driver,
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
    fn nested_crates_under_artifact_roots_are_errors() {
        let err = classify("service/drive/helper/Cargo.toml").unwrap_err();
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
            "phoxal-driver-drive",
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
}
