use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use crate::catalog::generate::{
    GenerateOptions, InputMode, build_catalog_revision, catalog_artifact_id, default_catalog_path,
    workspace_relative_path,
};
use crate::catalog::schema::{CatalogEntry, CatalogRevision};
use crate::catalog::verify::verify_catalog_path;
use crate::release::package;
use crate::release::plan::load_release_plan;
use crate::workspace::{OfficialArtifact, Workspace, require_nonempty_artifacts};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long, value_name = "PATH")]
    pub catalog: Option<PathBuf>,
    #[arg(long, value_name = "DIR", default_value = "target/xtask/release")]
    pub package_dir: PathBuf,
    #[arg(long = "target", value_name = "TRIPLE")]
    pub targets: Vec<String>,
    #[arg(long, value_name = "PATH")]
    pub release_plan: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub previous_catalog: Option<PathBuf>,
    /// Check a catalog generated with `catalog generate --metadata-only`.
    /// This covers every official artifact and validates schema ids without
    /// building release tarballs on each PR.
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
    let package_dir = package::workspace_relative_out_dir(&workspace, &args.package_dir);
    let host_triple = package::host_triple(workspace.root())?;
    let target_triples = if args.targets.is_empty() {
        vec![host_triple.clone()]
    } else {
        args.targets.clone()
    };
    let release_plan = args
        .release_plan
        .as_deref()
        .map(|path| load_release_plan(&workspace_relative_path(&workspace, path)))
        .transpose()?;
    let previous_catalog = args
        .previous_catalog
        .as_deref()
        .map(|path| verify_catalog_path(&workspace_relative_path(&workspace, path)))
        .transpose()?;
    let mode = if args.metadata_only {
        InputMode::MetadataOnly
    } else {
        InputMode::PackageOutputs
    };

    let actual = verify_catalog_path(&catalog_path)?;
    validate_coverage(&actual, artifacts)?;
    let expected = build_catalog_revision(
        &workspace,
        &GenerateOptions {
            package_dir,
            mode,
            target_triples,
            release_plan,
            previous_catalog,
        },
        &host_triple,
    )?;
    compare_catalogs(&actual, &expected)?;

    println!(
        "catalog check passed: {} covers {} official artifacts",
        catalog_path.display(),
        actual.entries.len()
    );
    Ok(())
}

pub(crate) fn validate_coverage(
    catalog: &CatalogRevision,
    artifacts: &[OfficialArtifact],
) -> Result<()> {
    let expected = artifacts
        .iter()
        .map(|artifact| catalog_artifact_id(artifact.kind, &artifact.id))
        .collect::<BTreeSet<_>>();
    let actual = catalog
        .entries
        .iter()
        .map(|entry| entry.artifact_id.clone())
        .collect::<BTreeSet<_>>();

    let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "catalog is missing official artifact entries: {}",
            missing.join(", ")
        );
    }

    let unexpected = actual.difference(&expected).cloned().collect::<Vec<_>>();
    if !unexpected.is_empty() {
        bail!(
            "catalog contains non-discovered official artifact entries: {}",
            unexpected.join(", ")
        );
    }

    Ok(())
}

pub(crate) fn compare_catalogs(actual: &CatalogRevision, expected: &CatalogRevision) -> Result<()> {
    let actual_entries = entries_by_id(actual)?;
    let expected_entries = entries_by_id(expected)?;

    for (artifact_id, expected_entry) in &expected_entries {
        let actual_entry = actual_entries
            .get(artifact_id)
            .with_context(|| format!("catalog is missing official artifact entry {artifact_id}"))?;
        if actual_entry.contract_uses != expected_entry.contract_uses {
            bail!("catalog schema_id drift for {artifact_id}; regenerate from packaged emit-apis");
        }
        if actual_entry != expected_entry {
            bail!(
                "catalog hand-edit/provenance drift for {artifact_id}; run `cargo xtask catalog generate`"
            );
        }
    }

    if actual.entries.len() != expected.entries.len() {
        bail!(
            "catalog entry count drift: recorded {}, expected {}",
            actual.entries.len(),
            expected.entries.len()
        );
    }

    let mut actual_without_entries = actual.clone();
    let mut expected_without_entries = expected.clone();
    actual_without_entries.entries.clear();
    expected_without_entries.entries.clear();
    if actual_without_entries != expected_without_entries {
        bail!(
            "catalog top-level provenance or integrity drift; run `cargo xtask catalog generate`"
        );
    }

    Ok(())
}

fn entries_by_id(catalog: &CatalogRevision) -> Result<BTreeMap<String, &CatalogEntry>> {
    let mut entries = BTreeMap::new();
    for entry in &catalog.entries {
        if entries.insert(entry.artifact_id.clone(), entry).is_some() {
            bail!(
                "catalog contains duplicate artifact_id {}",
                entry.artifact_id
            );
        }
    }
    Ok(entries)
}
