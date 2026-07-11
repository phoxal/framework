//! Restore release-plz's git-only artifact version ledger from a published
//! catalog.
//!
//! A build catalog is durable evidence that every official artifact at its
//! recorded workspace version was released from `build.commit`. This command
//! uses that evidence to recreate only missing `{package}-v{version}` tags.
//! Existing tags are never moved, and a workspace version absent from the
//! catalog is an error rather than an invented baseline. Two exact versions
//! whose refs are permanently blocked by a confirmed GitHub backend defect are
//! explicitly advanced by one patch; see [`CORRUPTED_TAG_SKIPS`].

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use crate::catalog::verify::verify_catalog_path;
use crate::workspace::Workspace;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CorruptedTagSkip {
    package: &'static str,
    catalog_version: &'static str,
    baseline_version: &'static str,
}

/// One-time remaps for refs corrupted by GitHub's create/delete ref cache bug.
///
/// GitHub permanently rejects the exact refs
/// `refs/tags/phoxal-service-asset-v0.19.6` and
/// `refs/tags/phoxal-tool-router-v0.1.5` with GH013 even though no ruleset
/// applies. See <https://github.com/orgs/community/discussions/183879>.
/// Matching both package and catalog version keeps the exception narrow: a
/// future catalog version is handled by the normal exact-version invariant.
const CORRUPTED_TAG_SKIPS: [CorruptedTagSkip; 2] = [
    CorruptedTagSkip {
        package: "phoxal/service-asset",
        catalog_version: "0.19.6",
        baseline_version: "0.19.7",
    },
    CorruptedTagSkip {
        package: "phoxal/tool-router",
        catalog_version: "0.1.5",
        baseline_version: "0.1.6",
    },
];

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Published catalog whose build commit and artifact versions are the
    /// authoritative recovery source.
    #[arg(long, value_name = "PATH")]
    pub catalog: PathBuf,
    /// Push newly-created tags atomically after creating them locally.
    #[arg(long)]
    pub push: bool,
    /// Git remote used by --push.
    #[arg(long, default_value = "origin")]
    pub remote: String,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let catalog = verify_catalog_path(&args.catalog)?;
    verify_commit(&catalog.build.commit)?;

    let published: BTreeSet<(&str, &str)> = catalog
        .artifacts
        .iter()
        .map(|artifact| (artifact.package.as_str(), artifact.version.as_str()))
        .collect();
    let mut missing_from_catalog = Vec::new();
    let mut tags_to_create = Vec::new();
    for artifact in workspace.official_artifacts() {
        let Some(baseline_version) = bootstrap_version(
            &published,
            artifact.package.as_str(),
            artifact.version.as_str(),
        ) else {
            missing_from_catalog.push(format!("{} v{}", artifact.package, artifact.version));
            continue;
        };
        let tag = artifact.release_tag_for_version(baseline_version);
        if !ref_exists(&format!("refs/tags/{tag}"))? {
            tags_to_create.push(tag);
        }
    }
    if !missing_from_catalog.is_empty() {
        bail!(
            "cannot bootstrap git-only tags: current workspace version(s) are absent from {}: {}",
            args.catalog.display(),
            missing_from_catalog.join(", ")
        );
    }

    for tag in &tags_to_create {
        git(&["tag", tag, &catalog.build.commit])
            .with_context(|| format!("failed to create {tag}"))?;
        println!("created {tag} at {}", catalog.build.commit);
    }

    if args.push && !tags_to_create.is_empty() {
        let mut arguments = vec!["push", "--atomic", args.remote.as_str()];
        let refs = tags_to_create
            .iter()
            .map(|tag| format!("refs/tags/{tag}"))
            .collect::<Vec<_>>();
        arguments.extend(refs.iter().map(String::as_str));
        git(&arguments).context("failed to push bootstrapped tags atomically")?;
        println!(
            "pushed {} bootstrapped tag(s) to {}",
            tags_to_create.len(),
            args.remote
        );
    } else if tags_to_create.is_empty() {
        println!("all official artifact tags already exist; nothing to bootstrap");
    } else {
        println!(
            "created {} local tag(s); rerun with --push to publish them",
            tags_to_create.len()
        );
    }
    Ok(())
}

/// Returns the version whose tag should seed release-plz, but only when the
/// catalog proves the current official `(package, version)` was published.
/// The two synthetic baseline versions are deliberately not required to exist
/// in the catalog: they are empty ledger waypoints anchored to the catalog's
/// proven build commit so release-plz advances past the uncreatable refs.
fn bootstrap_version<'a>(
    published: &BTreeSet<(&str, &str)>,
    package: &str,
    workspace_version: &'a str,
) -> Option<&'a str> {
    if !published.contains(&(package, workspace_version)) {
        return None;
    }

    CORRUPTED_TAG_SKIPS
        .iter()
        .find(|skip| skip.package == package && skip.catalog_version == workspace_version)
        .map(|skip| skip.baseline_version)
        .or(Some(workspace_version))
}

fn verify_commit(commit: &str) -> Result<()> {
    if commit.is_empty() {
        bail!("catalog build.commit is empty");
    }
    if !ref_exists(&format!("{commit}^{{commit}}"))? {
        bail!("catalog build commit {commit} does not exist in this checkout");
    }
    Ok(())
}

fn ref_exists(reference: &str) -> Result<bool> {
    let status = Command::new("git")
        .args(["rev-parse", "--quiet", "--verify", reference])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to inspect git reference {reference}"))?;
    Ok(status.success())
}

fn git(arguments: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args(arguments)
        .status()
        .with_context(|| format!("failed to run git {}", arguments.join(" ")))?;
    if !status.success() {
        bail!("git {} exited with {status}", arguments.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaps_only_the_two_exact_corrupted_catalog_versions() {
        let published = BTreeSet::from([
            ("phoxal/service-asset", "0.19.6"),
            ("phoxal/tool-router", "0.1.5"),
            ("phoxal/service-drive", "0.19.8"),
        ]);

        assert_eq!(
            bootstrap_version(&published, "phoxal/service-asset", "0.19.6"),
            Some("0.19.7")
        );
        assert_eq!(
            bootstrap_version(&published, "phoxal/tool-router", "0.1.5"),
            Some("0.1.6")
        );
        assert_eq!(
            bootstrap_version(&published, "phoxal/service-drive", "0.19.8"),
            Some("0.19.8")
        );
    }

    #[test]
    fn does_not_remap_nearby_versions_or_packages() {
        let published = BTreeSet::from([
            ("phoxal/service-asset", "0.19.7"),
            ("acme/service-asset", "0.19.6"),
            ("phoxal/tool-router", "0.1.6"),
        ]);

        for (package, version) in published.iter().copied() {
            assert_eq!(
                bootstrap_version(&published, package, version),
                Some(version)
            );
        }
    }

    #[test]
    fn still_requires_the_current_pair_in_the_catalog_for_skips() {
        let published = BTreeSet::from([
            ("phoxal/service-asset", "0.19.7"),
            ("phoxal/tool-router", "0.1.6"),
        ]);

        assert_eq!(
            bootstrap_version(&published, "phoxal/service-asset", "0.19.6"),
            None
        );
        assert_eq!(
            bootstrap_version(&published, "phoxal/tool-router", "0.1.5"),
            None
        );
    }
}
