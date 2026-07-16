use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
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
    /// Write release-plz-style Markdown for the selected official artifacts.
    /// The file lives outside the git tree in CI and is appended to the PR body.
    #[arg(long, value_name = "PATH")]
    pub notes_out: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let git = CliGitQuery;
    let selected = select_artifacts(&workspace, &args, &git)?;
    if let Some(path) = &args.notes_out {
        let releases = release_note_artifacts(&workspace, &selected, &git)?;
        write_release_notes(path, &releases, &git)?;
    }
    if selected.is_empty() {
        println!("release bump: no artifact version bumps needed");
        return Ok(());
    }
    require_nonempty_artifacts(&selected)?;

    bump_artifacts(&selected)?;
    sync_lockfile(&selected)?;
    println!("release bump: bumped {} artifact(s)", selected.len());
    Ok(())
}

fn select_artifacts(
    workspace: &Workspace,
    args: &Args,
    git: &dyn GitQuery,
) -> Result<Vec<OfficialArtifact>> {
    validate_monotonic_versions(workspace, git)?;

    if args.changed {
        return changed_artifacts(workspace, git);
    }

    let package = args
        .package
        .as_deref()
        .context("package is required unless --changed is present")?;
    Ok(vec![workspace.official_artifact(package)?.clone()])
}

/// Minimal seam over the git queries used by release selection and notes.
trait GitQuery {
    fn tag_exists(&self, tag: &str) -> Result<bool>;
    fn released_versions(&self, artifact: &OfficialArtifact) -> Result<Vec<String>>;
    fn changed_since(&self, tag: &str, path: &Path) -> Result<bool>;
    fn commit_subjects_since(&self, tag: &str, path: &Path) -> Result<Vec<String>>;
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

    fn released_versions(&self, artifact: &OfficialArtifact) -> Result<Vec<String>> {
        let prefix = release_tag_version_prefix(artifact)?;
        let output = Command::new("git")
            .args(["tag", "--list", &format!("{prefix}*")])
            .output()
            .with_context(|| format!("failed to list release tags for {}", artifact.package))?;
        if !output.status.success() {
            bail!(
                "git tag --list for {} failed with {}",
                artifact.package,
                output.status
            );
        }
        String::from_utf8(output.stdout)
            .context("git tag emitted non-UTF-8 tag names")?
            .lines()
            .map(|tag| {
                tag.strip_prefix(&prefix)
                    .with_context(|| format!("release tag {tag} does not start with {prefix}"))
                    .map(str::to_owned)
            })
            .collect()
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

    fn commit_subjects_since(&self, tag: &str, path: &Path) -> Result<Vec<String>> {
        let output = Command::new("git")
            .args([
                "log",
                "--no-merges",
                "--format=%s",
                &format!("{tag}..HEAD"),
                "--",
            ])
            .arg(path)
            .output()
            .with_context(|| format!("failed to spawn git log for {tag}..HEAD -- {path:?}"))?;
        if !output.status.success() {
            bail!(
                "git log {tag}..HEAD -- {path:?} failed with {}",
                output.status
            );
        }
        String::from_utf8(output.stdout)
            .context("git log emitted non-UTF-8 commit subjects")
            .map(|text| text.lines().map(str::to_owned).collect())
    }
}

fn release_tag_version_prefix(artifact: &OfficialArtifact) -> Result<String> {
    let current_tag = artifact.release_tag();
    current_tag
        .strip_suffix(&artifact.version)
        .map(str::to_owned)
        .with_context(|| {
            format!(
                "release tag {current_tag} does not end with artifact version {}",
                artifact.version
            )
        })
}

/// Release tags are immutable and are created before the catalog build. Reject
/// a package version reset while preparing the release PR, before a lower tag
/// can be merged and pushed.
fn validate_monotonic_versions(workspace: &Workspace, git: &dyn GitQuery) -> Result<()> {
    for artifact in workspace.official_artifacts() {
        let current = Version::parse(&artifact.version).with_context(|| {
            format!(
                "official artifact {} has invalid SemVer {}",
                artifact.package, artifact.version
            )
        })?;
        let highest = git
            .released_versions(artifact)
            .with_context(|| format!("failed to read release history for {}", artifact.package))?
            .into_iter()
            .map(|version| {
                Version::parse(&version)
                    .with_context(|| {
                        format!(
                            "release tag for {} has invalid SemVer {version}",
                            artifact.package
                        )
                    })
                    .map(|parsed| (version, parsed))
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .max_by(|left, right| left.1.cmp(&right.1));

        if let Some((highest_text, highest)) = highest
            && current < highest
        {
            bail!(
                "official artifact {} version {} is lower than released version {}; versions for \
                 the same package identity must be monotonic",
                artifact.package,
                artifact.version,
                highest_text
            );
        }
    }

    Ok(())
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

const NOTES_START: &str = "<!-- phoxal-artifact-releases:start -->";
const NOTES_END: &str = "<!-- phoxal-artifact-releases:end -->";

#[derive(Debug)]
struct ArtifactRelease {
    artifact: OfficialArtifact,
    from: String,
    to: String,
    from_tag: String,
    to_tag: String,
}

fn release_note_artifacts(
    workspace: &Workspace,
    selected: &[OfficialArtifact],
    git: &dyn GitQuery,
) -> Result<Vec<ArtifactRelease>> {
    let mut releases = Vec::new();
    for artifact in selected {
        let to = bump_patch_version(&artifact.version)?;
        releases.push(ArtifactRelease {
            artifact: artifact.clone(),
            from: artifact.version.clone(),
            to_tag: artifact.release_tag_for_version(&to),
            from_tag: artifact.release_tag(),
            to,
        });
    }

    // On an artifact-only PR rerun, the manifest is already bumped but its new
    // tag does not exist until the PR merges. Reconstruct that pending release
    // from the previous patch tag so new commits refresh the notes without a
    // second version bump.
    for artifact in workspace.official_artifacts() {
        if selected.iter().any(|item| item.package == artifact.package)
            || git.tag_exists(&artifact.release_tag())?
        {
            continue;
        }
        let Some(from) = previous_patch_version(&artifact.version)? else {
            continue;
        };
        let from_tag = artifact.release_tag_for_version(&from);
        if !git.tag_exists(&from_tag)? {
            continue;
        }
        releases.push(ArtifactRelease {
            artifact: artifact.clone(),
            from,
            to: artifact.version.clone(),
            from_tag,
            to_tag: artifact.release_tag(),
        });
    }
    releases.sort_by(|left, right| left.artifact.package.cmp(&right.artifact.package));
    Ok(releases)
}

fn write_release_notes(
    path: &Path,
    releases: &[ArtifactRelease],
    git: &dyn GitQuery,
) -> Result<()> {
    let notes = render_release_notes(releases, git)?;
    fs::write(path, notes).with_context(|| format!("failed to write {}", path.display()))
}

fn render_release_notes(releases: &[ArtifactRelease], git: &dyn GitQuery) -> Result<String> {
    if releases.is_empty() {
        return Ok(String::new());
    }
    let mut notes = format!("{NOTES_START}\n\n## 📦 Official artifact releases\n\n");
    for release in releases {
        let package_name = release.artifact.require_package_name()?;
        notes.push_str(&format!(
            "* `{package_name}`: {} -> {}\n",
            release.from, release.to
        ));
    }
    notes.push_str("\n<details><summary><i><b>Artifact changelog</b></i></summary><p>\n\n");

    for release in releases {
        let package_name = release.artifact.require_package_name()?;
        let subjects = git.commit_subjects_since(&release.from_tag, &release.artifact.crate_dir)?;
        let mut sections: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for subject in subjects {
            let (section, entry) = changelog_entry(&subject);
            sections.entry(section).or_default().push(entry);
        }

        notes.push_str(&format!(
            "## `{package_name}`\n\n<blockquote>\n\n## [{}](https://github.com/phoxal/framework/compare/{}...{})\n",
            release.to, release.from_tag, release.to_tag
        ));
        for section in ["Added", "Fixed", "Other"] {
            let Some(entries) = sections.get(section) else {
                continue;
            };
            notes.push_str(&format!("\n### {section}\n\n"));
            for entry in entries {
                notes.push_str(&format!("- {}\n", link_pull_request(entry)));
            }
        }
        if sections.is_empty() {
            notes.push_str(&format!(
                "\n### Other\n\n- Changes since `{}`\n",
                release.from_tag
            ));
        }
        notes.push_str("\n</blockquote>\n\n");
    }

    notes.push_str(&format!("</p></details>\n\n{NOTES_END}\n"));
    Ok(notes)
}

fn changelog_entry(subject: &str) -> (&'static str, String) {
    let (prefix, summary) = subject
        .split_once(": ")
        .filter(|(prefix, _)| !prefix.contains(' '))
        .unwrap_or(("", subject));
    let section = if prefix.starts_with("feat") {
        "Added"
    } else if prefix.starts_with("fix") {
        "Fixed"
    } else {
        "Other"
    };
    (section, summary.to_owned())
}

fn link_pull_request(entry: &str) -> String {
    let Some(start) = entry.rfind(" (#") else {
        return entry.to_owned();
    };
    let Some(number) = entry[start + 3..].strip_suffix(')') else {
        return entry.to_owned();
    };
    if number.is_empty() || !number.chars().all(|character| character.is_ascii_digit()) {
        return entry.to_owned();
    }
    format!(
        "{} ([#{number}](https://github.com/phoxal/framework/pull/{number}))",
        &entry[..start]
    )
}

/// Keep path-package versions in `Cargo.lock` in the same release PR as their
/// manifests. `cargo xtask` resolves the lockfile before xtask starts, so the
/// manifest edits above would otherwise remain stale until the next run.
fn sync_lockfile(artifacts: &[OfficialArtifact]) -> Result<()> {
    let package_names = artifacts
        .iter()
        .map(OfficialArtifact::require_package_name)
        .collect::<Result<Vec<_>>>()?;
    let mut command = Command::new("cargo");
    command.args(["update", "--offline"]);
    for package_name in &package_names {
        command.arg("-p").arg(package_name);
    }
    let status = command.status().with_context(|| {
        format!(
            "failed to spawn cargo update for {}",
            package_names.join(", ")
        )
    })?;
    if !status.success() {
        bail!(
            "cargo update failed for {} with {status}",
            package_names.join(", ")
        );
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

fn previous_patch_version(version: &str) -> Result<Option<String>> {
    let mut version = Version::parse(version)
        .with_context(|| format!("artifact version '{version}' is not valid semver"))?;
    if !version.pre.is_empty() || !version.build.is_empty() || version.patch == 0 {
        return Ok(None);
    }
    version.patch -= 1;
    Ok(Some(version.to_string()))
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

    fn release(artifact: &OfficialArtifact, from: &str, to: &str) -> ArtifactRelease {
        ArtifactRelease {
            artifact: artifact.clone(),
            from: from.to_owned(),
            to: to.to_owned(),
            from_tag: artifact.release_tag_for_version(from),
            to_tag: artifact.release_tag_for_version(to),
        }
    }

    struct FakeGitQuery {
        tags: HashMap<String, bool>,
        diffs: HashMap<String, bool>,
        subjects: HashMap<String, Vec<String>>,
    }

    impl GitQuery for FakeGitQuery {
        fn tag_exists(&self, tag: &str) -> Result<bool> {
            Ok(*self.tags.get(tag).unwrap_or(&false))
        }

        fn released_versions(&self, artifact: &OfficialArtifact) -> Result<Vec<String>> {
            let prefix = release_tag_version_prefix(artifact)?;
            Ok(self
                .tags
                .iter()
                .filter(|(_, exists)| **exists)
                .filter_map(|(tag, _)| tag.strip_prefix(&prefix).map(str::to_owned))
                .collect())
        }

        fn changed_since(&self, tag: &str, _path: &Path) -> Result<bool> {
            Ok(*self.diffs.get(tag).unwrap_or(&false))
        }

        fn commit_subjects_since(&self, tag: &str, _path: &Path) -> Result<Vec<String>> {
            Ok(self.subjects.get(tag).cloned().unwrap_or_default())
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
            subjects: HashMap::new(),
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
                subjects: HashMap::new(),
            },
        )?;
        assert!(selected.is_empty());
        Ok(())
    }

    #[test]
    fn release_selection_rejects_a_version_lower_than_a_published_tag() -> Result<()> {
        let reset = artifact(Path::new("/repo/service"), "safety");
        let workspace = Workspace::from_parts_for_tests(
            PathBuf::from("/repo"),
            PathBuf::from("/repo/target"),
            vec![reset.clone()],
        );
        let git = FakeGitQuery {
            tags: HashMap::from([
                (reset.release_tag(), true),
                (reset.release_tag_for_version("0.19.9"), true),
            ]),
            diffs: HashMap::new(),
            subjects: HashMap::new(),
        };
        let args = Args {
            package: None,
            changed: true,
            notes_out: None,
        };

        let error = select_artifacts(&workspace, &args, &git)
            .expect_err("a package version reset must fail before release selection");

        assert!(
            error.to_string().contains(
                "phoxal/service-safety version 0.1.0 is lower than released version 0.19.9"
            )
        );
        Ok(())
    }

    #[test]
    fn release_selection_accepts_the_highest_published_version() -> Result<()> {
        let mut safety = artifact(Path::new("/repo/service"), "safety");
        safety.version = "0.19.9".to_owned();
        let workspace = Workspace::from_parts_for_tests(
            PathBuf::from("/repo"),
            PathBuf::from("/repo/target"),
            vec![safety.clone()],
        );
        let git = FakeGitQuery {
            tags: HashMap::from([
                (safety.release_tag_for_version("0.1.0"), true),
                (safety.release_tag(), true),
            ]),
            diffs: HashMap::from([(safety.release_tag(), true)]),
            subjects: HashMap::new(),
        };
        let args = Args {
            package: None,
            changed: true,
            notes_out: None,
        };

        let selected = select_artifacts(&workspace, &args, &git)?;

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].package, "phoxal/service-safety");
        Ok(())
    }

    #[test]
    fn patch_bump_rejects_prerelease() {
        let error = bump_patch_version("0.2.0-alpha.1").unwrap_err();
        assert!(error.to_string().contains("pre-release"));
    }

    #[test]
    fn release_notes_include_only_selected_artifacts_and_full_changelog() -> Result<()> {
        let drive = artifact(Path::new("/repo/service"), "drive");
        let notes = render_release_notes(
            &[release(&drive, "0.1.0", "0.1.1")],
            &FakeGitQuery {
                tags: HashMap::new(),
                diffs: HashMap::new(),
                subjects: HashMap::from([(
                    drive.release_tag(),
                    vec![
                        "feat(drive): add traction control (#41)".to_owned(),
                        "fix: clamp invalid velocity (#42)".to_owned(),
                        "docs: explain tuning".to_owned(),
                    ],
                )]),
            },
        )?;

        assert!(notes.contains("`phoxal-service-drive`: 0.1.0 -> 0.1.1"));
        assert!(notes.contains("### Added"));
        assert!(notes.contains("add traction control ([#41]"));
        assert!(notes.contains("### Fixed"));
        assert!(notes.contains("clamp invalid velocity ([#42]"));
        assert!(notes.contains("### Other"));
        assert!(notes.contains("explain tuning"));
        assert!(!notes.contains("phoxal-service-map"));
        assert_eq!(notes.matches(NOTES_START).count(), 1);
        assert_eq!(notes.matches(NOTES_END).count(), 1);
        Ok(())
    }

    #[test]
    fn pending_bump_refreshes_notes_without_selecting_another_bump() -> Result<()> {
        let mut drive = artifact(Path::new("/repo/service"), "drive");
        drive.version = "0.1.1".to_owned();
        let workspace = Workspace::from_parts_for_tests(
            PathBuf::from("/repo"),
            PathBuf::from("/repo/target"),
            vec![drive.clone()],
        );
        let git = FakeGitQuery {
            tags: HashMap::from([
                (drive.release_tag_for_version("0.1.0"), true),
                (drive.release_tag(), false),
            ]),
            diffs: HashMap::new(),
            subjects: HashMap::from([(
                drive.release_tag_for_version("0.1.0"),
                vec!["fix(drive): include late follow-up (#43)".to_owned()],
            )]),
        };

        let releases = release_note_artifacts(&workspace, &[], &git)?;
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].from, "0.1.0");
        assert_eq!(releases[0].to, "0.1.1");
        let notes = render_release_notes(&releases, &git)?;
        assert!(notes.contains("include late follow-up ([#43]"));
        Ok(())
    }
}
