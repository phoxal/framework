//! The native build requirements CI installs, read through this crate's own
//! reader.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;
use phoxal::authoring::build_requirements::BuildRequirements;

/// The workspace this crate is part of: the parent of its own directory, since
/// the framework library sits at `phoxal/` under the workspace root.
fn workspace_root() -> Result<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .context("this crate's manifest directory has no workspace root")?
        .to_path_buf())
}

/// The apt packages CI installs are not a workflow author's choice: they are
/// the union of what the workspace's manifests declare they need.
///
/// It lives here rather than in the `cargo xtask policy` gate because the
/// declaration is only valid if *this* crate's reader accepts it, and the
/// runner deliberately links no framework crate.
#[test]
fn phoxal_metadata_namespace_is_valid_in_every_workspace_manifest() -> Result<()> {
    let workspace_root = workspace_root()?;
    let metadata = MetadataCommand::new()
        .manifest_path(workspace_root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .context("failed to read workspace metadata")?;

    let mut declared_packages = BTreeSet::new();
    for package in metadata.workspace_packages() {
        let path = package.manifest_path.clone().into_std_path_buf();
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let requirements = BuildRequirements::from_manifest(&source, &path.display().to_string())?;
        declared_packages.extend(requirements.apt.iter().cloned());
    }

    // A workspace that declares nothing must also install nothing: the
    // workflows carry no line at all rather than an empty one, so an
    // absent declaration reads as the empty union.
    let declared = declared_packages.into_iter().collect::<Vec<_>>();
    let declared_in = |workflow: &str, prefix: &str| -> Result<Vec<String>> {
        let source = fs::read_to_string(workspace_root.join(workflow))?;
        Ok(source
            .lines()
            .find_map(|line| line.trim().strip_prefix(prefix))
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect())
    };
    assert_eq!(
        declared_in(".github/workflows/ci.yml", "system-packages:")?,
        declared,
        "CI system-packages must equal the manifest-declared union"
    );
    assert_eq!(
        declared_in(
            ".github/workflows/release-plz.yml",
            "sudo apt-get install -y"
        )?,
        declared,
        "release packaging dependencies must equal the manifest-declared union"
    );
    Ok(())
}
