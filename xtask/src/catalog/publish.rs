use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use phoxal::catalog::Manifest;

use crate::catalog::verify::verify_catalog_path;
use crate::workspace::Workspace;

/// The one mutable "front door" release (docs #22, decision D25): a single,
/// fixed-tag, `--latest=true` release that hosts the catalog. It carries no
/// tarballs - those live in the per-artifact-version warehouse releases
/// `cargo xtask release cut`/`upload` create.
pub(crate) const DEFAULT_RELEASE_TAG: &str = "stable";

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(
        long,
        value_name = "PATH",
        default_value = "target/xtask/catalog/phoxal-artifacts.json"
    )]
    pub catalog: PathBuf,
    #[arg(
        long,
        value_name = "OWNER/REPO",
        env = "GITHUB_REPOSITORY",
        default_value = "phoxal/framework"
    )]
    pub repo: String,
    #[arg(long = "release-tag", value_name = "TAG", default_value = DEFAULT_RELEASE_TAG)]
    pub release_tag: String,
    /// Verify the catalog and print the publish plan without touching GitHub.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let catalog_path = if args.catalog.is_absolute() {
        args.catalog.clone()
    } else {
        workspace.root().join(&args.catalog)
    };
    let manifest = verify_catalog_path(&catalog_path)?;
    let latest_url = stable_release_catalog_url(&args.repo, &args.release_tag);
    let revision_filename = revision_filename(&manifest.revision);
    let body = grouped_release_body(&manifest);

    if args.dry_run {
        println!(
            "would publish catalog {} to {} (revision asset {})",
            manifest.revision, latest_url, revision_filename
        );
        println!("{body}");
        return Ok(());
    }

    let catalog_bytes = fs::read(&catalog_path)
        .with_context(|| format!("failed to read {}", catalog_path.display()))?;
    let staging_dir = workspace
        .root()
        .join("target/xtask/catalog-publish")
        .join(&args.release_tag);
    publish_catalog(
        &args.repo,
        &args.release_tag,
        &catalog_bytes,
        &manifest,
        &staging_dir,
        &GhReleases,
    )?;
    println!("published catalog {} to {}", manifest.revision, latest_url);
    Ok(())
}

/// The download URL the CLI reads the front-door catalog from
/// (`DEFAULT_CATALOG_URL` in the CLI, updated separately per docs #22).
pub(crate) fn stable_release_catalog_url(repo: &str, release_tag: &str) -> String {
    format!("https://github.com/{repo}/releases/download/{release_tag}/latest.json")
}

/// The immutable-by-convention per-revision asset filename appended to the
/// `stable` release alongside the overwritten `latest.json`.
fn revision_filename(revision: &str) -> String {
    let hex = revision.strip_prefix("sha256:").unwrap_or(revision);
    format!("phoxal-artifacts-{hex}.json")
}

/// Group catalog entries the way the `stable` release body presents them
/// (docs #22): one section per artifact kind, in a fixed display order,
/// entries sorted by package for a deterministic body. Kinds with no entries
/// in this revision are omitted rather than printed empty.
pub(crate) fn grouped_release_body(manifest: &Manifest) -> String {
    let mut body = format!("Phoxal artifact catalog - revision {}\n", manifest.revision);
    append_group(
        &mut body,
        "Services",
        manifest
            .services
            .iter()
            .map(|entry| (entry.package.as_str(), entry.version.as_str())),
    );
    append_group(
        &mut body,
        "Component drivers",
        manifest
            .drivers
            .iter()
            .map(|entry| (entry.package.as_str(), entry.version.as_str())),
    );
    append_group(
        &mut body,
        "Component assets",
        manifest
            .assets
            .iter()
            .map(|entry| (entry.package.as_str(), entry.version.as_str())),
    );
    append_group(
        &mut body,
        "Tools",
        manifest
            .tools
            .iter()
            .map(|entry| (entry.package.as_str(), entry.version.as_str())),
    );
    append_group(
        &mut body,
        "Simulators",
        manifest
            .simulators
            .iter()
            .map(|entry| (entry.package.as_str(), entry.version.as_str())),
    );
    body
}

fn append_group<'a>(
    body: &mut String,
    label: &str,
    entries: impl Iterator<Item = (&'a str, &'a str)>,
) {
    let mut sorted = entries.collect::<Vec<_>>();
    if sorted.is_empty() {
        return;
    }
    sorted.sort();

    body.push('\n');
    body.push_str(label);
    body.push('\n');
    for (package, version) in sorted {
        body.push_str(&format!("- {package} v{version}\n"));
    }
}

/// Seam over the `gh release` operations `catalog publish` needs, so the
/// ensure-then-clobber logic can be unit tested without a real repo or
/// network access.
trait Releases {
    /// Whether the front-door release already exists.
    fn release_exists(&self, repo: &str, tag: &str) -> Result<bool>;
    /// The commit SHA to create the release at, the first time it's cut.
    fn head_sha(&self) -> Result<String>;
    /// Create the published, `--latest=true` front-door release.
    fn create_release(
        &self,
        repo: &str,
        tag: &str,
        title: &str,
        sha: &str,
        notes: &str,
    ) -> Result<()>;
    /// Clobber-upload one asset (idempotent: re-publishing the same revision
    /// or refreshing `latest.json` overwrites in place).
    fn upload_asset(&self, repo: &str, tag: &str, path: &Path) -> Result<()>;
    /// Refresh the release body and (re-)assert it is the `--latest` release.
    fn edit_release(&self, repo: &str, tag: &str, notes: &str) -> Result<()>;
}

struct GhReleases;

impl Releases for GhReleases {
    fn release_exists(&self, repo: &str, tag: &str) -> Result<bool> {
        let status = Command::new("gh")
            .args(["release", "view", tag, "--repo", repo])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .with_context(|| format!("failed to spawn gh release view for {repo} {tag}"))?;
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

    fn create_release(
        &self,
        repo: &str,
        tag: &str,
        title: &str,
        sha: &str,
        notes: &str,
    ) -> Result<()> {
        let status = Command::new("gh")
            .args([
                "release", "create", tag, "--repo", repo, "--title", title, "--target", sha,
                "--latest", "--notes", notes,
            ])
            .status()
            .with_context(|| format!("failed to spawn gh release create for {repo} {tag}"))?;
        if !status.success() {
            bail!("gh release create failed for {repo} {tag} with status {status}");
        }
        Ok(())
    }

    fn upload_asset(&self, repo: &str, tag: &str, path: &Path) -> Result<()> {
        let status = Command::new("gh")
            .arg("release")
            .arg("upload")
            .arg(tag)
            .arg(path)
            .arg("--repo")
            .arg(repo)
            .arg("--clobber")
            .status()
            .with_context(|| format!("failed to spawn gh release upload for {repo} {tag}"))?;
        if !status.success() {
            bail!("gh release upload failed for {repo} {tag} with status {status}");
        }
        Ok(())
    }

    fn edit_release(&self, repo: &str, tag: &str, notes: &str) -> Result<()> {
        let status = Command::new("gh")
            .args([
                "release", "edit", tag, "--repo", repo, "--notes", notes, "--latest",
            ])
            .status()
            .with_context(|| format!("failed to spawn gh release edit for {repo} {tag}"))?;
        if !status.success() {
            bail!("gh release edit failed for {repo} {tag} with status {status}");
        }
        Ok(())
    }
}

/// Ensure the front-door release exists, then clobber-upload the revision
/// asset + `latest.json` and refresh the grouped body. `staging_dir` is where
/// the two upload files are written before handing them to `gh release
/// upload` (both are the same verified catalog bytes).
fn publish_catalog<R: Releases>(
    repo: &str,
    tag: &str,
    catalog_bytes: &[u8],
    manifest: &Manifest,
    staging_dir: &Path,
    releases: &R,
) -> Result<()> {
    let body = grouped_release_body(manifest);

    if !releases.release_exists(repo, tag)? {
        let sha = releases.head_sha()?;
        releases.create_release(repo, tag, "Phoxal artifact catalog", &sha, &body)?;
    }

    fs::create_dir_all(staging_dir)
        .with_context(|| format!("failed to create {}", staging_dir.display()))?;
    let revision_filename = revision_filename(&manifest.revision);
    let revision_path = staging_dir.join(&revision_filename);
    let latest_path = staging_dir.join("latest.json");
    fs::write(&revision_path, catalog_bytes)
        .with_context(|| format!("failed to write {}", revision_path.display()))?;
    fs::write(&latest_path, catalog_bytes)
        .with_context(|| format!("failed to write {}", latest_path.display()))?;

    releases.upload_asset(repo, tag, &revision_path)?;
    releases.upload_asset(repo, tag, &latest_path)?;
    releases.edit_release(repo, tag, &body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use phoxal::catalog::{Artifact, ArtifactEntry, AssetEntry, Channel};

    use super::*;

    fn artifact_entry(package: &str, version: &str) -> ArtifactEntry {
        let mut channels = BTreeMap::new();
        channels.insert(Channel::Stable, version.to_string());
        ArtifactEntry {
            package: package.to_string(),
            version: version.to_string(),
            api_generation: "y2026_1".to_string(),
            contracts: Vec::new(),
            config_schema: Some(serde_json::json!({ "type": "object" })),
            bus_abi: "phoxal-bus/v0".to_string(),
            artifacts: BTreeMap::<String, Artifact>::new(),
            channels,
            changed_contracts: Vec::new(),
        }
    }

    fn asset_entry(package: &str, version: &str) -> AssetEntry {
        let mut channels = BTreeMap::new();
        channels.insert(Channel::Stable, version.to_string());
        AssetEntry {
            package: package.to_string(),
            version: version.to_string(),
            artifacts: BTreeMap::new(),
            channels,
        }
    }

    fn fixture_manifest() -> Manifest {
        Manifest::new(
            vec![asset_entry("phoxal/component-ddsm115-assets", "0.1.0")],
            vec![
                artifact_entry("phoxal/service-map", "0.19.6"),
                artifact_entry("phoxal/service-drive", "0.19.6"),
            ],
            vec![artifact_entry("phoxal/component-ddsm115-driver", "0.1.5")],
            vec![artifact_entry("phoxal/tool-router", "0.1.5")],
            Vec::new(),
        )
        .finalize()
        .expect("fixture manifest finalizes")
    }

    #[test]
    fn stable_release_catalog_url_points_at_the_stable_release_latest_json() {
        assert_eq!(
            stable_release_catalog_url("phoxal/framework", "stable"),
            "https://github.com/phoxal/framework/releases/download/stable/latest.json"
        );
    }

    #[test]
    fn grouped_body_lists_entries_by_kind_sorted_and_omits_empty_groups() {
        let manifest = fixture_manifest();
        let body = grouped_release_body(&manifest);
        assert!(body.starts_with(&format!(
            "Phoxal artifact catalog - revision {}\n",
            manifest.revision
        )));
        assert!(body.contains(
            "\nServices\n\
- phoxal/service-drive v0.19.6\n\
- phoxal/service-map v0.19.6\n"
        ));
        assert!(body.contains(
            "\nComponent drivers\n\
- phoxal/component-ddsm115-driver v0.1.5\n"
        ));
        assert!(body.contains(
            "\nComponent assets\n\
- phoxal/component-ddsm115-assets v0.1.0\n"
        ));
        assert!(body.contains("\nTools\n- phoxal/tool-router v0.1.5\n"));
        assert!(!body.contains("Simulators"));
    }

    #[derive(Debug, Eq, PartialEq)]
    struct CreatedRelease {
        repo: String,
        tag: String,
        title: String,
        sha: String,
    }

    #[derive(Default)]
    struct FakeReleases {
        exists: bool,
        created: RefCell<Vec<CreatedRelease>>,
        uploaded: RefCell<Vec<(String, String, PathBuf)>>,
        edited: RefCell<Vec<(String, String, String)>>,
    }

    impl Releases for FakeReleases {
        fn release_exists(&self, _repo: &str, _tag: &str) -> Result<bool> {
            Ok(self.exists)
        }

        fn head_sha(&self) -> Result<String> {
            Ok("deadbeef".to_string())
        }

        fn create_release(
            &self,
            repo: &str,
            tag: &str,
            title: &str,
            sha: &str,
            _notes: &str,
        ) -> Result<()> {
            self.created.borrow_mut().push(CreatedRelease {
                repo: repo.to_string(),
                tag: tag.to_string(),
                title: title.to_string(),
                sha: sha.to_string(),
            });
            Ok(())
        }

        fn upload_asset(&self, repo: &str, tag: &str, path: &Path) -> Result<()> {
            self.uploaded.borrow_mut().push((
                repo.to_string(),
                tag.to_string(),
                path.to_path_buf(),
            ));
            Ok(())
        }

        fn edit_release(&self, repo: &str, tag: &str, notes: &str) -> Result<()> {
            self.edited
                .borrow_mut()
                .push((repo.to_string(), tag.to_string(), notes.to_string()));
            Ok(())
        }
    }

    #[test]
    fn publish_creates_the_release_when_missing_then_uploads_and_edits() -> Result<()> {
        let manifest = fixture_manifest();
        let releases = FakeReleases {
            exists: false,
            ..Default::default()
        };
        let dir = tempfile::tempdir().context("create tempdir")?;
        let staging_dir = dir.path().join("stable");

        publish_catalog(
            "phoxal/framework",
            "stable",
            b"catalog-bytes",
            &manifest,
            &staging_dir,
            &releases,
        )?;

        let created = releases.created.into_inner();
        assert_eq!(
            created,
            vec![CreatedRelease {
                repo: "phoxal/framework".to_string(),
                tag: "stable".to_string(),
                title: "Phoxal artifact catalog".to_string(),
                sha: "deadbeef".to_string(),
            }]
        );

        let uploaded = releases.uploaded.into_inner();
        assert_eq!(uploaded.len(), 2);
        let hex = manifest.revision.strip_prefix("sha256:").unwrap();
        assert_eq!(
            uploaded[0].2.file_name().unwrap().to_str().unwrap(),
            format!("phoxal-artifacts-{hex}.json")
        );
        assert_eq!(
            uploaded[1].2.file_name().unwrap().to_str().unwrap(),
            "latest.json"
        );
        for (_, _, path) in &uploaded {
            assert_eq!(fs::read(path)?, b"catalog-bytes");
        }

        let edited = releases.edited.into_inner();
        assert_eq!(edited.len(), 1);
        assert_eq!(edited[0].2, grouped_release_body(&manifest));
        Ok(())
    }

    #[test]
    fn publish_reuses_an_existing_release_without_recreating_it() -> Result<()> {
        let manifest = fixture_manifest();
        let releases = FakeReleases {
            exists: true,
            ..Default::default()
        };
        let dir = tempfile::tempdir().context("create tempdir")?;
        let staging_dir = dir.path().join("stable");

        publish_catalog(
            "phoxal/framework",
            "stable",
            b"catalog-bytes",
            &manifest,
            &staging_dir,
            &releases,
        )?;

        assert!(releases.created.into_inner().is_empty());
        assert_eq!(releases.uploaded.into_inner().len(), 2);
        assert_eq!(releases.edited.into_inner().len(), 1);
        Ok(())
    }
}
