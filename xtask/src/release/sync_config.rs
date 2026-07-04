use std::fs;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use crate::workspace::{OfficialArtifact, Workspace, require_nonempty_artifacts};

const RELEASE_PLZ_CONFIG: &str = "release-plz.toml";
const BEGIN_MARKER: &str = "# @generated begin phoxal artifact release-plz packages";
const END_MARKER: &str = "# @generated end phoxal artifact release-plz packages";

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Fail if release-plz.toml does not match workspace discovery.
    #[arg(long, conflicts_with = "write")]
    pub check: bool,
    /// Rewrite the managed package block in release-plz.toml.
    #[arg(long, conflicts_with = "check")]
    pub write: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Check,
    Write,
}

impl Args {
    fn mode(&self) -> Mode {
        if self.write { Mode::Write } else { Mode::Check }
    }
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let artifacts = workspace.official_artifacts();
    require_nonempty_artifacts(artifacts)?;

    let config_path = workspace.root().join(RELEASE_PLZ_CONFIG);
    let config = fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let synced = sync_config_text(&config, artifacts);

    if synced == config {
        println!(
            "{} release-plz artifact package block is in sync",
            config_path.display()
        );
        return Ok(());
    }

    match args.mode() {
        Mode::Check => bail!(
            "{} release-plz artifact package block is out of sync; run `cargo xtask release sync-config --write`",
            config_path.display()
        ),
        Mode::Write => {
            fs::write(&config_path, synced)
                .with_context(|| format!("failed to write {}", config_path.display()))?;
            println!("rewrote {}", config_path.display());
            Ok(())
        }
    }
}

fn sync_config_text(config: &str, artifacts: &[OfficialArtifact]) -> String {
    let block = managed_block(artifacts);
    let Some(begin) = config.find(BEGIN_MARKER) else {
        return append_managed_block(config, &block);
    };
    let Some(relative_end) = config[begin..].find(END_MARKER) else {
        return append_managed_block(config, &block);
    };
    let end = begin + relative_end + END_MARKER.len();
    let mut synced = String::with_capacity(config.len() + block.len());
    synced.push_str(&config[..begin]);
    synced.push_str(&block);
    synced.push_str(&config[end..]);
    synced
}

fn append_managed_block(config: &str, block: &str) -> String {
    let mut synced = config.trim_end().to_string();
    synced.push_str("\n\n");
    synced.push_str(block);
    synced.push('\n');
    synced
}

fn managed_block(artifacts: &[OfficialArtifact]) -> String {
    let mut block = String::new();
    block.push_str(BEGIN_MARKER);
    block.push('\n');
    block.push_str(
        "# Managed by `cargo xtask release sync-config --write` from workspace discovery.\n",
    );
    block.push_str("# Artifact crates are git-only: release-plz owns versions, tags, changelogs, and GitHub releases, but never crates.io publish.\n");
    for artifact in artifacts {
        block.push('\n');
        block.push_str("[[package]]\n");
        block.push_str(&format!("name = \"{}\"\n", artifact.package_name));
        block.push_str("release = true\n");
        block.push_str("git_only = true\n");
        block.push_str("publish = false\n");
        block.push_str("semver_check = false\n");
    }
    block.push_str(END_MARKER);
    block
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::workspace::{ArtifactKind, PhoxalPackageMetadata};

    use super::*;

    fn artifact(package_name: &str, kind: ArtifactKind, id: &str) -> OfficialArtifact {
        OfficialArtifact {
            package_name: package_name.to_string(),
            kind,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::new(),
            bin_name: package_name.to_string(),
            id: id.to_string(),
            metadata: PhoxalPackageMetadata::default(),
        }
    }

    #[test]
    fn managed_block_contains_git_only_package_scope() {
        let block = managed_block(&[artifact(
            "phoxal-service-drive",
            ArtifactKind::Service,
            "drive",
        )]);

        assert!(block.contains("name = \"phoxal-service-drive\""));
        assert!(block.contains("release = true"));
        assert!(block.contains("git_only = true"));
        assert!(block.contains("publish = false"));
    }

    #[test]
    fn sync_replaces_existing_block() {
        let config = "[workspace]\nrelease_always = false\n\n# @generated begin phoxal artifact release-plz packages\nold\n# @generated end phoxal artifact release-plz packages\n";
        let synced = sync_config_text(
            config,
            &[artifact(
                "phoxal-driver-ddsm115",
                ArtifactKind::Driver,
                "ddsm115",
            )],
        );

        assert!(!synced.contains("old"));
        assert!(synced.contains("name = \"phoxal-driver-ddsm115\""));
        assert!(synced.ends_with('\n'));
    }

    #[test]
    fn sync_appends_missing_block() {
        let synced = sync_config_text(
            "[workspace]\nrelease_always = false\n",
            &[artifact("phoxal-tool-router", ArtifactKind::Tool, "router")],
        );

        assert!(synced.contains(BEGIN_MARKER));
        assert!(synced.contains("name = \"phoxal-tool-router\""));
    }
}
