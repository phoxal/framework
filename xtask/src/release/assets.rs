//! `cargo xtask release assets` - packages one or more [`ArtifactKind::Component`]
//! crates' asset bundles (`component.yaml`,
//! `simulation.yaml`, `structure.urdf`, `meshes/` - design doc
//! `organization/tmp/ci-release-refactor/design.md` §9). No cargo build, no
//! binary, no `--target` flag: this is a plain file-tarball job, so any host
//! runner works (`crate::workspace::runner_for_target`'s
//! [`ASSETS_SCOPE`] arm).
//!
//! Kept as a separate verb from `release package` (which is binaries-only):
//! the two outputs have unrelated build steps, and the workflow's assets job
//! runs once per release train rather than once per binary-target matrix row.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use crate::release::package;
use crate::workspace::{OfficialArtifact, Workspace, require_nonempty_artifacts};

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// One or more Component package names to package as asset bundles.
    #[arg(value_name = "PACKAGE", required = true)]
    pub packages: Vec<String>,
    #[arg(long, value_name = "DIR", default_value = "target/xtask/release")]
    pub out: PathBuf,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let selected = select_artifacts(&workspace, &args)?;
    require_nonempty_artifacts(&selected)?;
    let out_dir = package::workspace_relative_out_dir(&workspace, &args.out);
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("failed to create output directory {}", out_dir.display()))?;

    let mut failures = Vec::new();
    for artifact in &selected {
        match package::package_component_asset_bundle(artifact, &out_dir) {
            Ok(packaged) => println!(
                "packaged {} v{} asset bundle -> {}, {}",
                artifact.package,
                artifact.version,
                packaged.tarball.display(),
                packaged.checksum.display(),
            ),
            Err(err) => failures.push(format!("{}: {err:#}", artifact.package)),
        }
    }
    if !failures.is_empty() {
        bail!(
            "failed to package {} of {} asset bundle(s):\n{}",
            failures.len(),
            selected.len(),
            failures.join("\n")
        );
    }

    Ok(())
}

fn select_artifacts(workspace: &Workspace, args: &Args) -> Result<Vec<OfficialArtifact>> {
    if args.packages.is_empty() {
        bail!("at least one PACKAGE is required");
    }

    args.packages
        .iter()
        .map(|package_name| {
            let artifact = workspace.official_artifact(package_name)?;
            if !artifact.kind.ships_assets() {
                bail!(
                    "{} is a {} package; only a Component package ships an \
                     asset bundle",
                    artifact.package,
                    artifact.kind
                );
            }
            Ok(artifact.clone())
        })
        .collect()
}
