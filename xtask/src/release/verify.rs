use std::process::Command;

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;
use clap::Args as ClapArgs;

use crate::release::suite;
use crate::workspace::Workspace;

/// The pure source/train gate: no packaged artifacts are read or produced
/// here. Staged-artifact verification and `suite.json` generation are
/// `release suite`'s job.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Exact release tag, checked against the workspace train version when
    /// given.
    #[arg(long)]
    pub tag: Option<String>,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let train = suite::workspace_version(&workspace)?;
    verify_uniform_versions(&train)?;
    run_cargo(
        workspace.root(),
        &[
            "test",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "--test-threads=1",
        ],
    )?;
    run_cargo(
        workspace.root(),
        &["check", "--workspace", "--all-targets", "--all-features"],
    )?;
    crate::coherence::run(crate::coherence::Args {})?;

    if let Some(tag) = args.tag {
        if tag != format!("v{train}") {
            bail!("release tag {tag} does not match workspace train v{train}");
        }
    }
    println!("framework train v{train} release verification passed");
    Ok(())
}

fn verify_uniform_versions(train: &str) -> Result<()> {
    let metadata = MetadataCommand::new().no_deps().exec()?;
    let mut mismatches = Vec::new();
    for package in metadata.workspace_packages() {
        if package.version.to_string() != train {
            mismatches.push(format!("{}={}", package.name, package.version));
        }
    }
    if !mismatches.is_empty() {
        bail!(
            "workspace train {train} has version mismatches: {}",
            mismatches.join(", ")
        );
    }
    Ok(())
}

fn run_cargo(root: &std::path::Path, args: &[&str]) -> Result<()> {
    let status = Command::new("cargo")
        .args(args)
        .current_dir(root)
        .status()
        .with_context(|| format!("failed to run cargo {}", args.join(" ")))?;
    if !status.success() {
        bail!("cargo {} failed with {status}", args.join(" "));
    }
    Ok(())
}
