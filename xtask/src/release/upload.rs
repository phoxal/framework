use std::collections::BTreeSet;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use crate::release::github;
use crate::release::package::{self, PackagedOutput};
use crate::workspace::Workspace;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long, value_name = "PACKAGE")]
    pub package: String,
    #[arg(long, value_name = "TRIPLE")]
    pub target: String,
    #[arg(long, value_name = "DIR", default_value = "target/xtask/release")]
    pub package_dir: std::path::PathBuf,
    #[arg(long, value_name = "OWNER/REPO", env = "GITHUB_REPOSITORY")]
    pub repo: String,
    #[arg(long, value_name = "TAG")]
    pub tag: Option<String>,
    /// Validate files and release identity without calling GitHub.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let artifact = workspace.official_artifact(&args.package)?;
    package::validate_supported_target(artifact, &args.target)?;
    let package_dir = package::workspace_relative_out_dir(&workspace, &args.package_dir);
    let output = package::read_packaged_output(artifact, &package_dir, &args.target)?;
    let tag = args.tag.unwrap_or_else(|| artifact.release_tag());

    if args.dry_run {
        println!(
            "would clobber-upload release assets for {} target {} to {}/{}: {}, {}",
            artifact.package,
            args.target,
            args.repo,
            tag,
            output.tarball_name,
            output.checksum_name
        );
        return Ok(());
    }

    upload_assets(&args.repo, &tag, &output)
}

pub(crate) fn github_release_asset_names(repo: &str, tag: &str) -> Result<BTreeSet<String>> {
    github::release_asset_names(repo, tag)
}

fn upload_assets(repo: &str, tag: &str, output: &PackagedOutput) -> Result<()> {
    let status = Command::new("gh")
        .arg("release")
        .arg("upload")
        .arg(tag)
        .arg(&output.tarball)
        .arg(&output.checksum)
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
