//! `cargo xtask catalog check` - structural-only gate (design doc
//! `organization/tmp/ci-release-refactor/design.md` §4.4).
//!
//! The catalog is a full archive of every published version, not a single
//! interoperating set, so this command does not (and cannot) check
//! coherence/interop - that is a property of a specific deployment
//! (`robot.yaml`'s pinned versions) and belongs to the CLI's `phoxal check`
//! at deploy time (out of scope for this refactor). What is checked here:
//!
//! - the file parses as `phoxal.catalog/v0` and satisfies
//!   [`super::verify::validate_structure`];
//! - **coverage**: every artifact `Workspace::discover` finds has its
//!   *current* version present in the catalog, so nothing built this run is
//!   silently missing from a partial run.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args as ClapArgs;

use super::generate::{default_catalog_path, workspace_relative_path};
use super::model::Catalog;
use super::verify::{load_catalog, validate_structure};
use crate::workspace::{OfficialArtifact, Workspace, require_nonempty_artifacts};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long, value_name = "PATH")]
    pub catalog: Option<PathBuf>,
    /// Check a catalog generated with `catalog generate --metadata-only`:
    /// entries legitimately carry no `targets`/`assets` output yet.
    #[arg(long)]
    pub metadata_only: bool,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let artifacts = workspace.official_artifacts();
    require_nonempty_artifacts(artifacts)?;

    let catalog_path = args
        .catalog
        .as_deref()
        .map(|path| workspace_relative_path(&workspace, path))
        .unwrap_or_else(|| default_catalog_path(&workspace));

    let catalog = load_catalog(&catalog_path)?;
    validate_structure(&catalog, args.metadata_only)?;
    validate_coverage(&catalog, artifacts)?;

    println!(
        "catalog check passed: {} covers {} artifact entries ({} official artifacts)",
        catalog_path.display(),
        catalog.artifacts.len(),
        artifacts.len()
    );
    Ok(())
}

/// Every artifact the workspace currently discovers must have its *current*
/// `(package, version)` present in the catalog. Older/other-package versions
/// carried forward from a previous run are not checked here - the catalog is
/// a superset, and coverage only cares that nothing was silently dropped.
pub(crate) fn validate_coverage(catalog: &Catalog, artifacts: &[OfficialArtifact]) -> Result<()> {
    let present: BTreeSet<(&str, &str)> = catalog
        .artifacts
        .iter()
        .map(|entry| (entry.package.as_str(), entry.version.as_str()))
        .collect();

    let missing: Vec<String> = artifacts
        .iter()
        .filter(|artifact| {
            !present.contains(&(artifact.package.as_str(), artifact.version.as_str()))
        })
        .map(|artifact| format!("{} v{}", artifact.package, artifact.version))
        .collect();

    if !missing.is_empty() {
        bail!(
            "catalog is missing the current version of: {}",
            missing.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::catalog::model::{Artifact, Blob, BuildProvenance, Heads};
    use crate::workspace::ArtifactKind;
    use std::collections::BTreeMap;

    fn official(package: &str, version: &str) -> OfficialArtifact {
        OfficialArtifact {
            package: package.to_string(),
            package_name: Some("irrelevant".to_string()),
            kind: ArtifactKind::Service,
            version: version.to_string(),
            crate_dir: PathBuf::new(),
            bin_name: Some("irrelevant".to_string()),
            id: "drive".to_string(),
            metadata: Default::default(),
        }
    }

    fn catalog_with(package: &str, version: &str) -> Catalog {
        let mut targets = BTreeMap::new();
        targets.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            Blob {
                url: "https://example.invalid/a.tar.zst".to_string(),
                sha256: "a".repeat(64),
                size: 1,
            },
        );
        Catalog::new(
            BuildProvenance {
                tag: "build-1".to_string(),
                run_number: 1,
                run_id: 1,
                commit: "c".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
            vec![Artifact {
                package: package.to_string(),
                version: version.to_string(),
                contracts: Vec::new(),
                config_schema: None,
                targets,
                assets: None,
            }],
            Heads::empty(),
        )
    }

    #[test]
    fn coverage_passes_when_current_version_is_present() {
        let artifacts = vec![official("phoxal/service-drive", "0.1.0")];
        let catalog = catalog_with("phoxal/service-drive", "0.1.0");
        validate_coverage(&catalog, &artifacts).expect("covered");
    }

    #[test]
    fn coverage_fails_when_current_version_is_missing() {
        let artifacts = vec![official("phoxal/service-drive", "0.2.0")];
        // Catalog only carries the older, already-published version.
        let catalog = catalog_with("phoxal/service-drive", "0.1.0");
        let err = validate_coverage(&catalog, &artifacts).unwrap_err();
        assert!(err.to_string().contains("phoxal/service-drive v0.2.0"));
    }

    #[test]
    fn coverage_fails_when_package_is_entirely_absent() {
        let artifacts = vec![official("phoxal/service-drive", "0.1.0")];
        let catalog = Catalog::new(
            BuildProvenance {
                tag: "build-1".to_string(),
                run_number: 1,
                run_id: 1,
                commit: "c".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
            Vec::new(),
            Heads::empty(),
        );
        let err = validate_coverage(&catalog, &artifacts).unwrap_err();
        assert!(err.to_string().contains("phoxal/service-drive v0.1.0"));
    }
}
