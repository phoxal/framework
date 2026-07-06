use std::io::{self, Write};

use crate::workspace::{Workspace, require_nonempty_artifacts};
use anyhow::Result;

#[derive(Debug, clap::Args)]
pub struct Args {
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let artifacts = workspace.official_artifacts();
    require_nonempty_artifacts(artifacts)?;

    if args.json {
        serde_json::to_writer_pretty(io::stdout().lock(), artifacts)?;
        println!();
    } else {
        print_table(artifacts)?;
    }

    Ok(())
}

fn print_table(artifacts: &[crate::workspace::OfficialArtifact]) -> Result<()> {
    const NO_BIN: &str = "-";
    let package_width = artifacts
        .iter()
        .map(|artifact| artifact.package.len())
        .chain([7])
        .max()
        .expect("width iterator is nonempty");
    let kind_width = artifacts
        .iter()
        .map(|artifact| artifact.kind.to_string().len())
        .chain([4])
        .max()
        .expect("width iterator is nonempty");
    let version_width = artifacts
        .iter()
        .map(|artifact| artifact.version.len())
        .chain([7])
        .max()
        .expect("width iterator is nonempty");
    let bin_width = artifacts
        .iter()
        .map(|artifact| artifact.bin_name.as_deref().unwrap_or(NO_BIN).len())
        .chain([3])
        .max()
        .expect("width iterator is nonempty");

    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    writeln!(
        stdout,
        "{:<package_width$}  {:<kind_width$}  {:<version_width$}  {:<bin_width$}  CRATE_DIR",
        "PACKAGE", "KIND", "VERSION", "BIN",
    )?;
    for artifact in artifacts {
        writeln!(
            stdout,
            "{:<package_width$}  {:<kind_width$}  {:<version_width$}  {:<bin_width$}  {}",
            artifact.package,
            artifact.kind,
            artifact.version,
            artifact.bin_name.as_deref().unwrap_or(NO_BIN),
            artifact.crate_dir.display(),
        )?;
    }

    Ok(())
}
