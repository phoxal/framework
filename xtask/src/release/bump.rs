use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use semver::Version;
use toml_edit::{DocumentMut, value};

use crate::workspace::{
    ASSETS_VERSION_FILE, OfficialArtifact, Workspace, require_nonempty_artifacts,
};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(
        value_name = "PACKAGE",
        required_unless_present_any = ["all", "changed"],
        conflicts_with_all = ["all", "changed"]
    )]
    pub package: Option<String>,
    /// Bump every discovered artifact crate.
    #[arg(long, conflicts_with = "changed")]
    pub all: bool,
    /// Bump every discovered artifact whose crate directory changed since its
    /// current version's release tag. Cheap (git diff only, no cargo package):
    /// artifact crates are no longer release-plz packages (see release-plz.toml's
    /// header), so their version bumps are computed here instead.
    #[arg(long, conflicts_with = "all")]
    pub changed: bool,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let selected = select_artifacts(&workspace, &args)?;
    if selected.is_empty() {
        println!("release bump: no artifact version bumps needed");
        return Ok(());
    }
    require_nonempty_artifacts(&selected)?;

    let mut bumped = 0usize;
    for artifact in &selected {
        let (from, to) = if artifact.kind.has_crate() {
            let manifest_path = artifact.crate_dir.join("Cargo.toml");
            bump_manifest_version(&manifest_path)
                .with_context(|| format!("failed to bump {}", manifest_path.display()))?
        } else {
            let version_path = artifact.crate_dir.join(ASSETS_VERSION_FILE);
            bump_assets_version_file(&version_path)
                .with_context(|| format!("failed to bump {}", version_path.display()))?
        };
        println!("bumped {}: {} -> {}", artifact.package, from, to);
        bumped += 1;
    }
    println!("release bump: bumped {bumped} artifact(s)");

    Ok(())
}

fn select_artifacts(workspace: &Workspace, args: &Args) -> Result<Vec<OfficialArtifact>> {
    if args.all {
        return Ok(workspace.official_artifacts().to_vec());
    }
    if args.changed {
        return changed_artifacts(workspace, &CliGitQuery);
    }

    let package_name = args
        .package
        .as_deref()
        .context("package is required unless --all or --changed is present")?;
    Ok(vec![workspace.official_artifact(package_name)?.clone()])
}

/// Minimal seam over the git queries `--changed` needs, so the tag-missing /
/// diff-clean / diff-dirty decision table can be unit tested without a real repo.
trait GitQuery {
    /// Whether `tag` exists in the repository.
    fn tag_exists(&self, tag: &str) -> Result<bool>;
    /// Whether `path` (relative to the repo root) changed between `tag` and
    /// `HEAD`, ignoring anything under one of `excludes` (paths relative to
    /// `path`, e.g. `driver` or `assets.version` for a `component_assets`
    /// bundle - docs #21: the driver crate and xtask-internal version file are
    /// not part of the asset bundle's own release contents).
    fn changed_since(&self, tag: &str, path: &Path, excludes: &[&str]) -> Result<bool>;
}

struct CliGitQuery;

impl GitQuery for CliGitQuery {
    fn tag_exists(&self, tag: &str) -> Result<bool> {
        let status = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/tags/{tag}"))
            .status()
            .with_context(|| format!("failed to spawn git rev-parse for tag {tag}"))?;
        Ok(status.success())
    }

    fn changed_since(&self, tag: &str, path: &Path, excludes: &[&str]) -> Result<bool> {
        let mut command = Command::new("git");
        command
            .args(["diff", "--quiet", &format!("{tag}..HEAD"), "--"])
            .arg(path);
        for exclude in excludes {
            command.arg(format!(":(exclude){}", path.join(exclude).display()));
        }
        let status = command
            .status()
            .with_context(|| format!("failed to spawn git diff for {tag}..HEAD -- {path:?}"))?;
        // `git diff --quiet` exits 0 when there is no difference, 1 when there is.
        // Any other exit code (e.g. the tag or path doesn't resolve) is a real error.
        match status.code() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => bail!("git diff --quiet {tag}..HEAD -- {path:?} exited abnormally: {status}"),
        }
    }
}

/// Decide whether an artifact needs a version bump under `--changed`.
///
/// - The tag for the artifact's current manifest version doesn't exist yet:
///   that version is still pending release (e.g. bumped by a prior run but not
///   yet tagged) - skip, nothing to do.
/// - The tag exists and the crate directory is unchanged since that tag: the
///   released contents still match - skip.
/// - The tag exists and the crate directory changed since that tag: bump.
fn should_bump(tag_exists: bool, changed_since_tag: bool) -> bool {
    tag_exists && changed_since_tag
}

/// Paths under a `component_assets` bundle's `crate_dir`
/// (`component/<id>/`) that are excluded from its `--changed` diff: `driver/`
/// is the separate `ComponentDriver` package's release contents, and
/// `assets.version` is xtask-internal release metadata, not a runtime asset
/// (docs #21).
const COMPONENT_ASSETS_CHANGED_EXCLUDES: [&str; 2] = ["driver", ASSETS_VERSION_FILE];

fn changed_artifacts(workspace: &Workspace, git: &dyn GitQuery) -> Result<Vec<OfficialArtifact>> {
    let mut selected = Vec::new();
    for artifact in workspace.official_artifacts() {
        let tag = artifact.release_tag();
        let tag_exists = git
            .tag_exists(&tag)
            .with_context(|| format!("failed to check tag {tag}"))?;
        if !tag_exists {
            println!(
                "skip {}: tag {tag} not found (release pending)",
                artifact.package
            );
            continue;
        }

        let excludes: &[&str] = if artifact.kind.has_crate() {
            &[]
        } else {
            &COMPONENT_ASSETS_CHANGED_EXCLUDES
        };
        let changed = git
            .changed_since(&tag, &artifact.crate_dir, excludes)
            .with_context(|| format!("failed to diff {} since {tag}", artifact.package))?;
        if should_bump(tag_exists, changed) {
            selected.push(artifact.clone());
        } else {
            println!("skip {}: no changes since {tag}", artifact.package);
        }
    }
    Ok(selected)
}

fn bump_manifest_version(manifest_path: &std::path::Path) -> Result<(String, String)> {
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

/// Bumps a [`ArtifactKind::ComponentAssets`] package's version, which lives in
/// `component/<id>/assets.version` rather than a `Cargo.toml` `[package]
/// version` (a `component_assets` bundle has no crate - docs #21).
fn bump_assets_version_file(version_path: &std::path::Path) -> Result<(String, String)> {
    let text = fs::read_to_string(version_path)
        .with_context(|| format!("failed to read {}", version_path.display()))?;
    let current = text.trim().to_string();
    if current.is_empty() {
        bail!("{} is empty", version_path.display());
    }
    let bumped = bump_patch_version(&current)?;
    fs::write(version_path, format!("{bumped}\n"))
        .with_context(|| format!("failed to write {}", version_path.display()))?;
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
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;

    use crate::workspace::{ArtifactKind, PhoxalPackageMetadata};

    use super::*;

    #[test]
    fn patch_bump_increments_patch() -> Result<()> {
        assert_eq!(bump_patch_version("0.19.1")?, "0.19.2");
        assert_eq!(bump_patch_version("1.2.3")?, "1.2.4");
        Ok(())
    }

    #[test]
    fn patch_bump_rejects_prerelease() {
        let err = bump_patch_version("0.2.0-alpha.1").unwrap_err();
        assert!(err.to_string().contains("pre-release"));
    }

    // --changed decision table: tag-missing -> skip, diff-clean -> skip, diff-dirty -> bump.
    #[test]
    fn should_bump_decision_table() {
        assert!(
            !should_bump(false, false),
            "tag missing, no diff: still pending release, skip"
        );
        assert!(
            !should_bump(false, true),
            "tag missing: version is pending release regardless of diff, skip"
        );
        assert!(
            !should_bump(true, false),
            "tag exists, no diff since tag: released contents still match, skip"
        );
        assert!(
            should_bump(true, true),
            "tag exists and crate dir changed since tag: bump"
        );
    }

    fn artifact(package_name: &str, crate_dir: &str) -> OfficialArtifact {
        // `package_name` here is a full crate name like `phoxal-service-drive`;
        // strip the leading `phoxal-` to recover the provider-qualified `package`
        // (`phoxal/service-drive`) these fixtures need for `release_tag()`.
        let package = format!("phoxal/{}", package_name.strip_prefix("phoxal-").unwrap());
        OfficialArtifact {
            package,
            package_name: Some(package_name.to_string()),
            kind: ArtifactKind::Service,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::from(crate_dir),
            bin_name: Some(package_name.to_string()),
            id: package_name.to_string(),
            metadata: PhoxalPackageMetadata::default(),
        }
    }

    fn component_assets_artifact(id: &str, crate_dir: &str) -> OfficialArtifact {
        OfficialArtifact {
            package: crate::workspace::package_identity(ArtifactKind::ComponentAssets, id),
            package_name: None,
            kind: ArtifactKind::ComponentAssets,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::from(crate_dir),
            bin_name: None,
            id: id.to_string(),
            metadata: PhoxalPackageMetadata::default(),
        }
    }

    /// Fake `GitQuery` driven entirely by in-memory maps, so `changed_artifacts`
    /// can be exercised without a real repo. Also records the `excludes` each
    /// call received, so tests can assert `component_assets` bundles diff with
    /// `driver`/`assets.version` excluded while crate-backed artifacts diff with
    /// none.
    struct FakeGitQuery {
        tags: HashMap<String, bool>,
        diffs: HashMap<String, bool>,
        excludes_seen: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl GitQuery for FakeGitQuery {
        fn tag_exists(&self, tag: &str) -> Result<bool> {
            Ok(*self.tags.get(tag).unwrap_or(&false))
        }

        fn changed_since(&self, tag: &str, _path: &Path, excludes: &[&str]) -> Result<bool> {
            self.excludes_seen.borrow_mut().push((
                tag.to_string(),
                excludes.iter().map(|value| value.to_string()).collect(),
            ));
            Ok(*self.diffs.get(tag).unwrap_or(&false))
        }
    }

    #[test]
    fn changed_artifacts_selects_only_tagged_and_dirty_crates() {
        let workspace = Workspace::from_parts_for_tests(
            PathBuf::from("/repo"),
            PathBuf::from("/repo/target"),
            vec![
                artifact("phoxal-service-drive", "/repo/service/drive"),
                artifact("phoxal-service-map", "/repo/service/map"),
                artifact("phoxal-tool-router", "/repo/tool/router"),
            ],
        );

        let git = FakeGitQuery {
            tags: HashMap::from([
                // drive: tagged and changed since -> bump
                ("phoxal-service-drive-v0.1.0".to_string(), true),
                // map: tagged but unchanged since -> skip
                ("phoxal-service-map-v0.1.0".to_string(), true),
                // router: no tag yet (pending release) -> skip
            ]),
            diffs: HashMap::from([
                ("phoxal-service-drive-v0.1.0".to_string(), true),
                ("phoxal-service-map-v0.1.0".to_string(), false),
            ]),
            excludes_seen: RefCell::new(Vec::new()),
        };

        let selected = changed_artifacts(&workspace, &git).expect("changed_artifacts");
        let names: Vec<&str> = selected
            .iter()
            .map(|artifact| artifact.package.as_str())
            .collect();
        assert_eq!(names, vec!["phoxal/service-drive"]);
    }

    #[test]
    fn changed_artifacts_includes_component_assets_and_excludes_driver_and_version_file() {
        let workspace = Workspace::from_parts_for_tests(
            PathBuf::from("/repo"),
            PathBuf::from("/repo/target"),
            vec![component_assets_artifact(
                "ddsm115",
                "/repo/component/ddsm115",
            )],
        );

        let git = FakeGitQuery {
            tags: HashMap::from([("phoxal-component-ddsm115-assets-v0.1.0".to_string(), true)]),
            diffs: HashMap::from([("phoxal-component-ddsm115-assets-v0.1.0".to_string(), true)]),
            excludes_seen: RefCell::new(Vec::new()),
        };

        let selected = changed_artifacts(&workspace, &git).expect("changed_artifacts");
        assert_eq!(
            selected
                .iter()
                .map(|a| a.package.as_str())
                .collect::<Vec<_>>(),
            vec!["phoxal/component-ddsm115-assets"]
        );
        assert_eq!(
            git.excludes_seen.into_inner(),
            vec![(
                "phoxal-component-ddsm115-assets-v0.1.0".to_string(),
                vec!["driver".to_string(), ASSETS_VERSION_FILE.to_string()],
            )]
        );
    }

    #[test]
    fn bump_assets_version_file_bumps_patch() -> Result<()> {
        let dir = tempfile::tempdir().context("create tempdir")?;
        let path = dir.path().join("assets.version");
        fs::write(&path, "0.1.0\n")?;

        let (from, to) = bump_assets_version_file(&path)?;

        assert_eq!(from, "0.1.0");
        assert_eq!(to, "0.1.1");
        assert_eq!(fs::read_to_string(&path)?.trim(), "0.1.1");
        Ok(())
    }
}
