//! Restore release-plz's git-only artifact version ledger from a published
//! catalog.
//!
//! A build catalog is durable evidence that every official artifact at its
//! recorded workspace version was released from `build.commit`. This command
//! uses that evidence to recreate only missing `{package}-v{version}` tags.
//! Existing tags are never moved, and a workspace version absent from the
//! catalog is an error rather than an invented baseline.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use crate::catalog::verify::verify_catalog_path;
use crate::workspace::Workspace;

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
        if !published.contains(&(artifact.package.as_str(), artifact.version.as_str())) {
            missing_from_catalog.push(format!("{} v{}", artifact.package, artifact.version));
            continue;
        }
        let tag = artifact.release_tag();
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
