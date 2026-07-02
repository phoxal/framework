use std::fmt;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use cargo_metadata::{MetadataCommand, TargetKind};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Service,
    Driver,
    // Planned by the xtask command tree, but no workspace package maps here yet.
    #[allow(dead_code)]
    Tool,
    // Planned by the xtask command tree, but no workspace package maps here yet.
    #[allow(dead_code)]
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

#[derive(Clone, Debug, Serialize)]
pub struct OfficialArtifact {
    pub package_name: String,
    pub kind: ArtifactKind,
    pub version: String,
    pub crate_dir: PathBuf,
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
        let metadata = MetadataCommand::new()
            .no_deps()
            .exec()
            .context("failed to read cargo metadata")?;
        let root = metadata.workspace_root.clone().into_std_path_buf();
        let target_dir = metadata.target_directory.clone().into_std_path_buf();
        let mut official_artifacts = Vec::new();

        for package in metadata.workspace_packages() {
            let package_name = package.name.to_string();
            let Some((kind, id)) = classify_package(&package_name) else {
                continue;
            };
            let crate_dir = package
                .manifest_path
                .parent()
                .with_context(|| format!("{package_name} manifest has no parent directory"))?
                .to_path_buf()
                .into_std_path_buf();
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
}

fn classify_package(package_name: &str) -> Option<(ArtifactKind, String)> {
    package_name
        .strip_prefix("phoxal-service-")
        .map(|id| (ArtifactKind::Service, id.to_string()))
        .or_else(|| {
            package_name
                .strip_prefix("phoxal-driver-")
                .map(|id| (ArtifactKind::Driver, id.to_string()))
        })
}

pub fn require_nonempty_artifacts(artifacts: &[OfficialArtifact]) -> Result<()> {
    if artifacts.is_empty() {
        bail!("no official release artifacts were discovered");
    }
    Ok(())
}
