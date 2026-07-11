use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use semver::Version;
use toml_edit::{DocumentMut, value};

use crate::workspace::{OfficialArtifact, Workspace, require_nonempty_artifacts};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(value_name = "PACKAGE", required_unless_present = "changed")]
    pub package: Option<String>,
    /// Bump every official artifact whose own crate directory changed since
    /// its current version tag. This is intentionally a git-only comparison:
    /// workspace lockfile or library churn cannot select an artifact.
    #[arg(long, conflicts_with = "package")]
    pub changed: bool,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let selected = select_artifacts(&workspace, &args, &CliGitQuery)?;
    if selected.is_empty() {
        println!("release bump: no artifact version bumps needed");
        return Ok(());
    }
    require_nonempty_artifacts(&selected)?;

    bump_artifacts(&selected)?;
    println!("release bump: bumped {} artifact(s)", selected.len());
    Ok(())
}

fn select_artifacts(
    workspace: &Workspace,
    args: &Args,
    git: &dyn GitQuery,
) -> Result<Vec<OfficialArtifact>> {
    if args.changed {
        return changed_artifacts(workspace, git);
    }

    let package = args
        .package
        .as_deref()
        .context("package is required unless --changed is present")?;
    Ok(vec![workspace.official_artifact(package)?.clone()])
}

/// Minimal seam over the two git queries used by `--changed`.
trait GitQuery {
    fn tag_exists(&self, tag: &str) -> Result<bool>;
    fn changed_since(&self, tag: &str, path: &Path) -> Result<bool>;
}

struct CliGitQuery;

impl GitQuery for CliGitQuery {
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

    fn changed_since(&self, tag: &str, path: &Path) -> Result<bool> {
        let status = Command::new("git")
            .args(["diff", "--quiet", &format!("{tag}..HEAD"), "--"])
            .arg(path)
            .status()
            .with_context(|| format!("failed to spawn git diff for {tag}..HEAD -- {path:?}"))?;
        match status.code() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => bail!("git diff --quiet {tag}..HEAD -- {path:?} failed with {status}"),
        }
    }
}

fn changed_artifacts(workspace: &Workspace, git: &dyn GitQuery) -> Result<Vec<OfficialArtifact>> {
    let mut selected = Vec::new();
    for artifact in workspace.official_artifacts() {
        let tag = artifact.release_tag();
        if !git
            .tag_exists(&tag)
            .with_context(|| format!("failed to check tag {tag}"))?
        {
            println!(
                "skip {}: tag {tag} not found (release pending)",
                artifact.package
            );
            continue;
        }

        if git
            .changed_since(&tag, &artifact.crate_dir)
            .with_context(|| format!("failed to diff {} since {tag}", artifact.package))?
        {
            selected.push(artifact.clone());
        } else {
            println!("skip {}: no changes since {tag}", artifact.package);
        }
    }
    Ok(selected)
}

fn bump_artifacts(artifacts: &[OfficialArtifact]) -> Result<()> {
    for artifact in artifacts {
        let manifest_path = artifact.crate_dir.join("Cargo.toml");
        let (from, to) = bump_manifest_version(&manifest_path)
            .with_context(|| format!("failed to bump {}", manifest_path.display()))?;
        println!("bumped {}: {from} -> {to}", artifact.package);
    }
    Ok(())
}

fn bump_manifest_version(manifest_path: &Path) -> Result<(String, String)> {
    let text = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let mut document = text
        .parse::<DocumentMut>()
        .with_context(|| format!("{} is not valid TOML", manifest_path.display()))?;
    let package = document["package"]
        .as_table_mut()
        .with_context(|| format!("{} has no [package] table", manifest_path.display()))?;
    let current = package
        .get("version")
        .and_then(|item| item.as_str())
        .with_context(|| format!("{} [package] version is missing", manifest_path.display()))?
        .to_string();
    let bumped = bump_patch_version(&current)?;
    package["version"] = value(bumped.clone());
    fs::write(manifest_path, document.to_string())
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;
    Ok((current, bumped))
}

fn bump_patch_version(version: &str) -> Result<String> {
    let mut version = Version::parse(version).with_context(|| {
        format!("artifact version '{version}' is not valid semver and cannot be bumped")
    })?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        bail!("artifact version '{version}' has pre-release or build metadata; bump it manually");
    }
    version.patch += 1;
    Ok(version.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use crate::workspace::{ArtifactKind, PhoxalPackageMetadata, package_identity};

    use super::*;

    fn artifact(root: &Path, id: &str) -> OfficialArtifact {
        let package_name = format!("phoxal-service-{id}");
        OfficialArtifact {
            package: package_identity(ArtifactKind::Service, id),
            package_name: Some(package_name.clone()),
            kind: ArtifactKind::Service,
            version: "0.1.0".to_string(),
            crate_dir: root.join(id),
            bin_name: Some(package_name),
            id: id.to_string(),
            metadata: PhoxalPackageMetadata::default(),
        }
    }

    fn write_manifest(artifact: &OfficialArtifact) -> Result<()> {
        fs::create_dir_all(&artifact.crate_dir)?;
        fs::write(
            artifact.crate_dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{}\"\nversion = \"{}\"\n",
                artifact.require_package_name()?,
                artifact.version
            ),
        )?;
        Ok(())
    }

    struct FakeGitQuery {
        tags: HashMap<String, bool>,
        diffs: HashMap<String, bool>,
    }

    impl GitQuery for FakeGitQuery {
        fn tag_exists(&self, tag: &str) -> Result<bool> {
            Ok(*self.tags.get(tag).unwrap_or(&false))
        }

        fn changed_since(&self, tag: &str, _path: &Path) -> Result<bool> {
            Ok(*self.diffs.get(tag).unwrap_or(&false))
        }
    }

    #[test]
    fn changing_one_artifact_bumps_only_its_manifest() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let drive = artifact(temp.path(), "drive");
        let map = artifact(temp.path(), "map");
        let router = artifact(temp.path(), "router");
        for artifact in [&drive, &map, &router] {
            write_manifest(artifact)?;
        }
        let workspace = Workspace::from_parts_for_tests(
            temp.path().to_path_buf(),
            temp.path().join("target"),
            vec![drive.clone(), map.clone(), router.clone()],
        );
        let git = FakeGitQuery {
            tags: HashMap::from([
                (drive.release_tag(), true),
                (map.release_tag(), true),
                (router.release_tag(), true),
            ]),
            diffs: HashMap::from([
                (drive.release_tag(), true),
                (map.release_tag(), false),
                (router.release_tag(), false),
            ]),
        };

        let selected = changed_artifacts(&workspace, &git)?;
        assert_eq!(
            selected
                .iter()
                .map(|artifact| artifact.package.as_str())
                .collect::<Vec<_>>(),
            vec!["phoxal/service-drive"]
        );
        bump_artifacts(&selected)?;

        assert!(fs::read_to_string(drive.crate_dir.join("Cargo.toml"))?.contains("0.1.1"));
        assert!(fs::read_to_string(map.crate_dir.join("Cargo.toml"))?.contains("0.1.0"));
        assert!(fs::read_to_string(router.crate_dir.join("Cargo.toml"))?.contains("0.1.0"));
        Ok(())
    }

    #[test]
    fn missing_current_tag_means_the_bump_is_already_pending() -> Result<()> {
        let artifact = artifact(Path::new("/repo/service"), "drive");
        let workspace = Workspace::from_parts_for_tests(
            PathBuf::from("/repo"),
            PathBuf::from("/repo/target"),
            vec![artifact],
        );
        let selected = changed_artifacts(
            &workspace,
            &FakeGitQuery {
                tags: HashMap::new(),
                diffs: HashMap::new(),
            },
        )?;
        assert!(selected.is_empty());
        Ok(())
    }

    #[test]
    fn patch_bump_rejects_prerelease() {
        let error = bump_patch_version("0.2.0-alpha.1").unwrap_err();
        assert!(error.to_string().contains("pre-release"));
    }
}
