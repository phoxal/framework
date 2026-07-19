use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use serde::{Deserialize, Serialize};

use crate::workspace::{OfficialArtifact, Workspace, require_nonempty_artifacts};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Where to write the release-plz-compatible JSON document consumed by
    /// `cargo xtask release plan`.
    #[arg(
        long,
        value_name = "PATH",
        default_value = "target/xtask/artifact-releases.json"
    )]
    pub out: PathBuf,
    /// Report missing tags without creating or pushing them.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let artifacts = workspace.official_artifacts();
    require_nonempty_artifacts(artifacts)?;

    let releases = cut_artifacts(artifacts, args.dry_run, &CliGit)?;
    write_release_document(&args.out, &releases)?;
    let action = if args.dry_run { "would cut" } else { "cut" };
    println!(
        "release cut: {action} {} of {} artifact tag(s) -> {}",
        releases.len(),
        artifacts.len(),
        args.out.display()
    );
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct CutRelease {
    pub package_name: String,
    pub version: String,
    pub tag: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ReleaseDocument {
    pub releases: Vec<CutRelease>,
}

trait Git {
    fn tag_exists(&self, tag: &str) -> Result<bool>;
    fn head_sha(&self) -> Result<String>;
    fn create_and_push_tag(&self, tag: &str, sha: &str) -> Result<()>;
}

struct CliGit;

impl Git for CliGit {
    fn tag_exists(&self, tag: &str) -> Result<bool> {
        let status = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/tags/{tag}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("failed to spawn git rev-parse for tag {tag}"))?;
        Ok(status.success())
    }

    fn head_sha(&self) -> Result<String> {
        let output = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .context("failed to spawn git rev-parse HEAD")?;
        if !output.status.success() {
            bail!(
                "git rev-parse HEAD failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn create_and_push_tag(&self, tag: &str, sha: &str) -> Result<()> {
        let status = Command::new("git")
            .args(["tag", tag, sha])
            .status()
            .with_context(|| format!("failed to spawn git tag {tag} {sha}"))?;
        if !status.success() {
            bail!("git tag {tag} {sha} failed with {status}");
        }

        let status = Command::new("git")
            .args(["push", "origin"])
            .arg(format!("refs/tags/{tag}"))
            .status()
            .with_context(|| format!("failed to spawn git push for tag {tag}"))?;
        if !status.success() {
            bail!("git push origin refs/tags/{tag} failed with {status}");
        }
        Ok(())
    }
}

fn cut_artifacts(
    artifacts: &[OfficialArtifact],
    dry_run: bool,
    git: &dyn Git,
) -> Result<Vec<CutRelease>> {
    let sha = git.head_sha()?;
    let mut releases = Vec::new();
    for artifact in artifacts {
        let tag = artifact.release_tag();
        if git
            .tag_exists(&tag)
            .with_context(|| format!("failed to check tag {tag}"))?
        {
            println!("skip {}: tag {tag} already exists", artifact.package);
            continue;
        }

        if dry_run {
            println!("would cut {} ({tag})", artifact.package);
        } else {
            git.create_and_push_tag(&tag, &sha)
                .with_context(|| format!("failed to cut {}", artifact.package))?;
        }
        releases.push(CutRelease {
            package_name: artifact.require_package_name()?.to_string(),
            version: artifact.version.clone(),
            tag,
        });
    }
    Ok(releases)
}

pub(crate) fn write_release_document(path: &Path, releases: &[CutRelease]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut json = serde_json::to_string_pretty(&ReleaseDocument {
        releases: releases.to_vec(),
    })
    .context("failed to serialize artifact release document")?;
    json.push('\n');
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;

    use crate::workspace::{ArtifactKind, PhoxalPackageMetadata, package_identity};

    use super::*;

    fn artifact(id: &str, version: &str) -> OfficialArtifact {
        let package_name = format!("phoxal-service-{id}");
        OfficialArtifact {
            package: package_identity(ArtifactKind::Service, id),
            package_name: Some(package_name.clone()),
            kind: ArtifactKind::Service,
            version: version.to_string(),
            crate_dir: PathBuf::from(format!("/repo/service/{id}")),
            bin_name: Some(package_name),
            id: id.to_string(),
            metadata: PhoxalPackageMetadata::default(),
        }
    }

    #[derive(Default)]
    struct FakeGit {
        tags: HashMap<String, bool>,
        created: RefCell<Vec<(String, String)>>,
    }

    impl Git for FakeGit {
        fn tag_exists(&self, tag: &str) -> Result<bool> {
            Ok(*self.tags.get(tag).unwrap_or(&false))
        }

        fn head_sha(&self) -> Result<String> {
            Ok("deadbeef".to_string())
        }

        fn create_and_push_tag(&self, tag: &str, sha: &str) -> Result<()> {
            self.created
                .borrow_mut()
                .push((tag.to_string(), sha.to_string()));
            Ok(())
        }
    }

    #[test]
    fn cut_skips_existing_tags_and_pushes_only_missing_tags() -> Result<()> {
        let artifacts = vec![artifact("drive", "0.19.7"), artifact("map", "0.4.2")];
        let git = FakeGit {
            tags: HashMap::from([("phoxal-service-drive-v0.19.7".to_string(), true)]),
            ..Default::default()
        };

        let releases = cut_artifacts(&artifacts, false, &git)?;

        assert_eq!(
            releases,
            vec![CutRelease {
                package_name: "phoxal-service-map".to_string(),
                version: "0.4.2".to_string(),
                tag: "phoxal-service-map-v0.4.2".to_string(),
            }]
        );
        assert_eq!(
            git.created.into_inner(),
            vec![(
                "phoxal-service-map-v0.4.2".to_string(),
                "deadbeef".to_string()
            )]
        );
        Ok(())
    }

    #[test]
    fn release_document_has_release_plz_shape() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("releases.json");
        write_release_document(
            &path,
            &[CutRelease {
                package_name: "phoxal-tool-bus".to_string(),
                version: "0.1.6".to_string(),
                tag: "phoxal-tool-bus-v0.1.6".to_string(),
            }],
        )?;

        let document: ReleaseDocument = serde_json::from_str(&fs::read_to_string(path)?)?;
        assert_eq!(document.releases[0].package_name, "phoxal-tool-bus");
        Ok(())
    }
}
