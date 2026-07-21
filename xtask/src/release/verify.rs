use std::process::Command;

use anyhow::{Context, Result, bail};
use cargo_metadata::MetadataCommand;
use clap::Args as ClapArgs;

use crate::suite;
use crate::workspace::Workspace;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Verify staged release artifacts and produce suite.json in addition to
    /// the source/train gates.
    #[arg(long)]
    pub staged: bool,
    #[arg(long, default_value = "target/xtask/release")]
    pub package_dir: std::path::PathBuf,
    #[arg(long, default_value = "target/xtask/suite.json")]
    pub out: std::path::PathBuf,
    #[arg(long)]
    pub tag: Option<String>,
    #[arg(long, env = "GITHUB_REPOSITORY", default_value = "phoxal/framework")]
    pub repo: String,
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

    if args.staged {
        let tag = args.tag.context("--tag is required with --staged")?;
        suite::run(suite::Args {
            package_dir: args.package_dir,
            out: args.out,
            tag,
            repo: args.repo,
        })?;
    } else if let Some(tag) = args.tag {
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
