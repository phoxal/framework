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
            "would upload immutable release assets for {} target {} to {}/{}: {}, {}, {}",
            artifact.package,
            args.target,
            args.repo,
            tag,
            output.tarball_name,
            output.checksum_name,
            output.metadata_name
        );
        return Ok(());
    }

    let existing = github_release_asset_names(&args.repo, &tag)?;
    ensure_assets_absent(&existing, &output)?;
    upload_assets(&args.repo, &tag, &output)
}

fn ensure_assets_absent(existing: &BTreeSet<String>, output: &PackagedOutput) -> Result<()> {
    let requested = [
        output.tarball_name.as_str(),
        output.checksum_name.as_str(),
        output.metadata_name.as_str(),
    ];
    let duplicates = requested
        .iter()
        .filter(|name| existing.contains(**name))
        .copied()
        .collect::<Vec<_>>();
    if !duplicates.is_empty() {
        bail!(
            "GitHub release already has immutable asset(s): {}",
            duplicates.join(", ")
        );
    }
    Ok(())
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
        .arg(&output.metadata)
        .arg("--repo")
        .arg(repo)
        .status()
        .with_context(|| format!("failed to spawn gh release upload for {repo} {tag}"))?;
    if !status.success() {
        bail!("gh release upload failed for {repo} {tag} with status {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output() -> PackagedOutput {
        PackagedOutput {
            tarball: "asset.tar.zst".into(),
            checksum: "asset.tar.zst.sha256".into(),
            metadata: "asset.emit-apis.json".into(),
            tarball_name: "asset.tar.zst".to_string(),
            checksum_name: "asset.tar.zst.sha256".to_string(),
            metadata_name: "asset.emit-apis.json".to_string(),
            tarball_sha256: "a".repeat(64),
            metadata_sha256: "b".repeat(64),
            emit_apis: package::parse_emit_apis_json(
                br#"{
  "schema": "phoxal.emit-apis/v0",
  "artifact": { "kind": "service", "id": "drive" },
  "framework": { "version": "0.21.0" },
  "api_version": "y2026_1",
  "participant_class": "checked",
  "bus_abi": "phoxal-bus/v0",
  "required_contracts": [
    {
      "api_version": "y2026_1",
      "schema_id": "0123456789abcdef",
      "family": "drive::Target",
      "topic": "drive/target",
      "direction": "publish"
    }
  ],
  "config_schema": { "type": "object" }
}"#,
                "drive",
                crate::workspace::ArtifactKind::Service,
            )
            .unwrap(),
        }
    }

    #[test]
    fn existing_asset_fails_immutable_upload() {
        let existing = ["asset.tar.zst".to_string()].into_iter().collect();
        let err = ensure_assets_absent(&existing, &output()).unwrap_err();
        assert!(err.to_string().contains("immutable asset"));
    }
}
