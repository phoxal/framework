use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use serde::Serialize;

use crate::workspace::{
    OfficialArtifact, Workspace, filesystem_safe_package, require_nonempty_artifacts,
};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long, value_name = "OWNER/REPO", env = "GITHUB_REPOSITORY")]
    pub repo: String,
    /// Where to write a release-plz-JSON-compatible document describing the
    /// artifacts this run cut, for `cargo xtask release plan` to consume.
    #[arg(
        long,
        value_name = "PATH",
        default_value = "target/xtask/release-plz.json"
    )]
    pub out: PathBuf,
    /// Print what would be cut without creating tags or releases.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    // Every official artifact is git-cut, including `component_assets` bundles:
    // they have no Cargo crate/version to tag, but they still need a git tag +
    // draft GitHub release to hold their packaged tarball (docs #21).
    let artifacts = workspace.official_artifacts().to_vec();
    require_nonempty_artifacts(&artifacts)?;

    let git = CliGit;
    let cut = cut_artifacts(&args.repo, &artifacts, args.dry_run, &git)?;

    write_release_plz_json(&args.out, &cut)?;

    let action = if args.dry_run { "would cut" } else { "cut" };
    println!(
        "release cut: {action} {} of {} artifact(s) -> {}",
        cut.len(),
        artifacts.len(),
        args.out.display()
    );
    for entry in &cut {
        println!("  {} ({})", entry.tag, entry.package);
    }

    Ok(())
}

/// One artifact this run created a tag + draft release for. `package` is the
/// provider-qualified public identity (`phoxal/component-ddsm115-assets`) and
/// is always present; `package_name` is the Cargo crate name
/// (release-plz-JSON-compatible key) and is `None` for a `component_assets`
/// bundle, which has no crate. `cargo xtask release plan` correlates by either
/// field (see `plan::build_release_plan`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CutRelease {
    package: String,
    package_name: Option<String>,
    version: String,
    tag: String,
}

/// Seam over the git/gh operations `cut` needs, so the skip/cut decision and the
/// output shape can be unit tested without a real repo or network access.
trait Git {
    /// Whether `tag` already exists (locally or on the remote - `cut` only ever
    /// checks the local clone, which in CI is always a fresh checkout of the
    /// target ref, so a local check is equivalent and avoids a network round trip).
    fn tag_exists(&self, tag: &str) -> Result<bool>;
    /// The commit SHA to cut the tag/release at.
    fn head_sha(&self) -> Result<String>;
    /// A short human-readable note for the release body: commits touching
    /// `crate_dir` since the last tag matching `tag_glob`, or `None` if this is
    /// the artifact's first release.
    fn release_notes(&self, tag_glob: &str, current_tag: &str, crate_dir: &Path) -> Result<String>;
    /// Create the git tag (at `sha`) and a draft, not-latest GitHub release for it.
    fn create_draft_release(
        &self,
        repo: &str,
        tag: &str,
        title: &str,
        sha: &str,
        notes: &str,
    ) -> Result<()>;
}

struct CliGit;

impl Git for CliGit {
    fn tag_exists(&self, tag: &str) -> Result<bool> {
        let status = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet"])
            .arg(format!("refs/tags/{tag}"))
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

    fn release_notes(&self, tag_glob: &str, current_tag: &str, crate_dir: &Path) -> Result<String> {
        let previous_tag = previous_tag(tag_glob, current_tag)?;
        let Some(previous_tag) = previous_tag else {
            return Ok("Initial release.".to_string());
        };

        let output = Command::new("git")
            .args(["log", "--oneline", &format!("{previous_tag}..HEAD"), "--"])
            .arg(crate_dir)
            .output()
            .with_context(|| format!("failed to spawn git log {previous_tag}..HEAD"))?;
        if !output.status.success() {
            bail!(
                "git log {previous_tag}..HEAD failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let log = String::from_utf8_lossy(&output.stdout);
        let commits = log.lines().count();
        Ok(format!(
            "{commits} commit(s) since {previous_tag}:\n\n{log}"
        ))
    }

    fn create_draft_release(
        &self,
        repo: &str,
        tag: &str,
        title: &str,
        sha: &str,
        notes: &str,
    ) -> Result<()> {
        let status = Command::new("gh")
            .args([
                "release",
                "create",
                tag,
                "--repo",
                repo,
                "--title",
                title,
                "--target",
                sha,
                "--draft",
                "--latest=false",
                "--notes",
                notes,
            ])
            .status()
            .with_context(|| format!("failed to spawn gh release create for {repo} {tag}"))?;
        if !status.success() {
            bail!("gh release create failed for {repo} {tag} with status {status}");
        }
        Ok(())
    }
}

/// Find the most recent existing tag matching `tag_glob` other than `current_tag`,
/// sorted by version (newest first). `git tag --list` with `--sort=-v:refname`
/// gives us this directly without needing to parse semver ourselves.
fn previous_tag(tag_glob: &str, current_tag: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["tag", "--list", "--sort=-v:refname", tag_glob])
        .output()
        .with_context(|| format!("failed to spawn git tag --list {tag_glob}"))?;
    if !output.status.success() {
        bail!(
            "git tag --list {tag_glob} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let tags = String::from_utf8_lossy(&output.stdout);
    Ok(tags
        .lines()
        .map(str::trim)
        .find(|tag| !tag.is_empty() && *tag != current_tag)
        .map(str::to_string))
}

/// Decide whether an artifact needs cutting: skip if its release tag already
/// exists (idempotent reconciliation - the same semantics `release_always` gave
/// release-plz), otherwise cut it.
fn should_cut(tag_exists: bool) -> bool {
    !tag_exists
}

fn cut_artifacts(
    repo: &str,
    artifacts: &[OfficialArtifact],
    dry_run: bool,
    git: &dyn Git,
) -> Result<Vec<CutRelease>> {
    let mut cut = Vec::new();
    for artifact in artifacts {
        let tag = artifact.release_tag();
        let tag_exists = git
            .tag_exists(&tag)
            .with_context(|| format!("failed to check tag {tag}"))?;

        if !should_cut(tag_exists) {
            println!("skip {}: tag {tag} already exists", artifact.package);
            continue;
        }

        if dry_run {
            println!("would cut {} ({tag})", artifact.package);
        } else {
            let sha = git.head_sha()?;
            let tag_glob = format!("{}-v*", filesystem_safe_package(&artifact.package));
            let notes = git.release_notes(&tag_glob, &tag, &artifact.crate_dir)?;
            let title = format!("{} v{}", artifact.package, artifact.version);
            git.create_draft_release(repo, &tag, &title, &sha, &notes)?;
        }

        cut.push(CutRelease {
            package: artifact.package.clone(),
            package_name: artifact.package_name.clone(),
            version: artifact.version.clone(),
            tag,
        });
    }
    Ok(cut)
}

/// Write a release-plz `release --output json`-compatible document: a `releases`
/// array of `{ package_name, version, tag }` objects. `cargo xtask release plan`
/// already parses exactly this shape (see `plan::released_packages`), so the
/// downstream plan/matrix/upload/gate/publish steps need no changes.
fn write_release_plz_json(path: &Path, cut: &[CutRelease]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    #[derive(Serialize)]
    struct Document<'a> {
        releases: &'a [CutRelease],
    }
    let mut json = serde_json::to_string_pretty(&Document { releases: cut })
        .context("failed to serialize release cut document")?;
    json.push('\n');
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::PathBuf;

    use crate::workspace::{ArtifactKind, PhoxalPackageMetadata};

    use super::*;

    fn artifact(package_name: &str, version: &str) -> OfficialArtifact {
        // `package_name` here is a full crate name like `phoxal-service-drive`;
        // strip the leading `phoxal-` to recover the provider-qualified `package`.
        let package = format!("phoxal/{}", package_name.strip_prefix("phoxal-").unwrap());
        OfficialArtifact {
            package,
            package_name: Some(package_name.to_string()),
            kind: ArtifactKind::Service,
            version: version.to_string(),
            crate_dir: PathBuf::from(format!("/repo/service/{package_name}")),
            bin_name: Some(package_name.to_string()),
            id: package_name.to_string(),
            metadata: PhoxalPackageMetadata::default(),
        }
    }

    fn component_assets_artifact(id: &str, version: &str) -> OfficialArtifact {
        OfficialArtifact {
            package: crate::workspace::package_identity(ArtifactKind::ComponentAssets, id),
            package_name: None,
            kind: ArtifactKind::ComponentAssets,
            version: version.to_string(),
            crate_dir: PathBuf::from(format!("/repo/component/{id}")),
            bin_name: None,
            id: id.to_string(),
            metadata: PhoxalPackageMetadata::default(),
        }
    }

    // cut decision table: tag exists -> skip, tag missing -> cut.
    #[test]
    fn should_cut_decision_table() {
        assert!(!should_cut(true), "tag already exists: skip, idempotent");
        assert!(should_cut(false), "tag missing: cut a new release");
    }

    #[derive(Default)]
    struct FakeGit {
        tags: HashMap<String, bool>,
        created: RefCell<Vec<(String, String, String)>>,
    }

    impl Git for FakeGit {
        fn tag_exists(&self, tag: &str) -> Result<bool> {
            Ok(*self.tags.get(tag).unwrap_or(&false))
        }

        fn head_sha(&self) -> Result<String> {
            Ok("deadbeef".to_string())
        }

        fn release_notes(
            &self,
            _tag_glob: &str,
            _current_tag: &str,
            _crate_dir: &Path,
        ) -> Result<String> {
            Ok("notes".to_string())
        }

        fn create_draft_release(
            &self,
            repo: &str,
            tag: &str,
            title: &str,
            _sha: &str,
            _notes: &str,
        ) -> Result<()> {
            self.created
                .borrow_mut()
                .push((repo.to_string(), tag.to_string(), title.to_string()));
            Ok(())
        }
    }

    #[test]
    fn cut_artifacts_skips_existing_tags_and_cuts_missing_ones() -> Result<()> {
        let artifacts = vec![
            artifact("phoxal-service-drive", "0.19.5"),
            artifact("phoxal-service-map", "0.19.5"),
        ];
        let git = FakeGit {
            tags: HashMap::from([("phoxal-service-drive-v0.19.5".to_string(), true)]),
            ..Default::default()
        };

        let cut = cut_artifacts("phoxal/framework", &artifacts, false, &git)?;

        assert_eq!(
            cut,
            vec![CutRelease {
                package: "phoxal/service-map".to_string(),
                package_name: Some("phoxal-service-map".to_string()),
                version: "0.19.5".to_string(),
                tag: "phoxal-service-map-v0.19.5".to_string(),
            }]
        );
        assert_eq!(
            git.created.into_inner(),
            vec![(
                "phoxal/framework".to_string(),
                "phoxal-service-map-v0.19.5".to_string(),
                "phoxal/service-map v0.19.5".to_string(),
            )]
        );
        Ok(())
    }

    #[test]
    fn cut_artifacts_dry_run_creates_nothing() -> Result<()> {
        let artifacts = vec![artifact("phoxal-tool-router", "0.1.5")];
        let git = FakeGit::default();

        let cut = cut_artifacts("phoxal/framework", &artifacts, true, &git)?;

        assert_eq!(cut.len(), 1);
        assert!(git.created.into_inner().is_empty());
        Ok(())
    }

    #[test]
    fn release_plz_json_document_round_trips_through_plan_parser() -> Result<()> {
        let cut = vec![CutRelease {
            package: "phoxal/service-drive".to_string(),
            package_name: Some("phoxal-service-drive".to_string()),
            version: "0.19.5".to_string(),
            tag: "phoxal-service-drive-v0.19.5".to_string(),
        }];
        let dir = tempfile::tempdir().context("create tempdir")?;
        let path = dir.path().join("release-plz.json");
        write_release_plz_json(&path, &cut)?;

        let workspace = Workspace::from_parts_for_tests(
            PathBuf::from("/repo"),
            PathBuf::from("/repo/target"),
            vec![artifact("phoxal-service-drive", "0.19.5")],
        );
        let text = fs::read_to_string(&path)?;
        let plan = crate::release::plan::build_release_plan(&workspace, &text)?;

        assert_eq!(plan.artifacts.len(), 1);
        assert_eq!(plan.artifacts[0].tag, "phoxal-service-drive-v0.19.5");
        Ok(())
    }

    #[test]
    fn component_assets_release_plz_json_round_trips_through_plan_parser_by_package() -> Result<()>
    {
        let cut = vec![CutRelease {
            package: "phoxal/component-ddsm115-assets".to_string(),
            package_name: None,
            version: "0.1.0".to_string(),
            tag: "phoxal-component-ddsm115-assets-v0.1.0".to_string(),
        }];
        let dir = tempfile::tempdir().context("create tempdir")?;
        let path = dir.path().join("release-plz.json");
        write_release_plz_json(&path, &cut)?;

        let workspace = Workspace::from_parts_for_tests(
            PathBuf::from("/repo"),
            PathBuf::from("/repo/target"),
            vec![component_assets_artifact("ddsm115", "0.1.0")],
        );
        let text = fs::read_to_string(&path)?;
        let plan = crate::release::plan::build_release_plan(&workspace, &text)?;

        assert_eq!(plan.artifacts.len(), 1);
        assert_eq!(
            plan.artifacts[0].tag,
            "phoxal-component-ddsm115-assets-v0.1.0"
        );
        assert_eq!(
            plan.artifacts[0].target_triples,
            vec![crate::workspace::TARGET_INDEPENDENT_SCOPE.to_string()]
        );
        Ok(())
    }

    #[test]
    fn cut_release_serializes_with_package_and_package_name_fields() -> Result<()> {
        let cut = CutRelease {
            package: "phoxal/component-ddsm115-driver".to_string(),
            package_name: Some("phoxal-component-ddsm115-driver".to_string()),
            version: "0.1.5".to_string(),
            tag: "phoxal-component-ddsm115-driver-v0.1.5".to_string(),
        };
        let json = serde_json::to_value(&cut)?;
        let object = json.as_object().context("expected object")?;
        assert!(object.contains_key("package"));
        assert!(object.contains_key("package_name"));
        assert!(object.contains_key("version"));
        assert!(object.contains_key("tag"));
        Ok(())
    }

    #[test]
    fn cut_release_serializes_component_assets_with_null_package_name() -> Result<()> {
        let cut = CutRelease {
            package: "phoxal/component-ddsm115-assets".to_string(),
            package_name: None,
            version: "0.1.0".to_string(),
            tag: "phoxal-component-ddsm115-assets-v0.1.0".to_string(),
        };
        let json = serde_json::to_value(&cut)?;
        let object = json.as_object().context("expected object")?;
        assert!(object["package_name"].is_null());
        Ok(())
    }

    #[test]
    fn cut_artifacts_includes_component_assets_with_no_crate() -> Result<()> {
        let artifacts = vec![component_assets_artifact("ddsm115", "0.1.0")];
        let git = FakeGit::default();

        let cut = cut_artifacts("phoxal/framework", &artifacts, false, &git)?;

        assert_eq!(
            cut,
            vec![CutRelease {
                package: "phoxal/component-ddsm115-assets".to_string(),
                package_name: None,
                version: "0.1.0".to_string(),
                tag: "phoxal-component-ddsm115-assets-v0.1.0".to_string(),
            }]
        );
        assert_eq!(
            git.created.into_inner(),
            vec![(
                "phoxal/framework".to_string(),
                "phoxal-component-ddsm115-assets-v0.1.0".to_string(),
                "phoxal/component-ddsm115-assets v0.1.0".to_string(),
            )]
        );
        Ok(())
    }
}
