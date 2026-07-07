use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use phoxal::catalog::{ArtifactEntry, AssetEntry, Manifest};

use crate::catalog::generate::{
    GenerateOptions, InputMode, build_catalog_revision, default_catalog_path,
    workspace_relative_path,
};
use crate::catalog::verify::verify_catalog_path;
use crate::release::package;
use crate::release::plan::load_release_plan;
use crate::workspace::{ArtifactKind, OfficialArtifact, Workspace, require_nonempty_artifacts};

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
        actual.total_entries()
    );
    Ok(())
}

/// The five (kind, array) pairs a [`Manifest`] carries, for coverage checks
/// that need to walk every array against the workspace's discovered official
/// artifacts by kind.
const RUNTIME_KINDS: [ArtifactKind; 4] = [
    ArtifactKind::Service,
    ArtifactKind::ComponentDriver,
    ArtifactKind::Tool,
    ArtifactKind::Simulator,
];

pub(crate) fn validate_coverage(manifest: &Manifest, artifacts: &[OfficialArtifact]) -> Result<()> {
    check_kind_coverage(
        ArtifactKind::ComponentAssets,
        artifacts,
        manifest.assets.iter().map(|entry| entry.package.as_str()),
    )?;
    for (kind, entries) in RUNTIME_KINDS.into_iter().zip([
        &manifest.services,
        &manifest.drivers,
        &manifest.tools,
        &manifest.simulators,
    ]) {
        check_kind_coverage(
            kind,
            artifacts,
            entries.iter().map(|entry| entry.package.as_str()),
        )?;
    }
    Ok(())
}

fn check_kind_coverage<'a>(
    kind: ArtifactKind,
    artifacts: &[OfficialArtifact],
    actual_packages: impl Iterator<Item = &'a str>,
) -> Result<()> {
    let expected = artifacts
        .iter()
        .filter(|artifact| artifact.kind == kind)
        .map(|artifact| artifact.package.as_str())
        .collect::<BTreeSet<_>>();
    let actual = actual_packages.collect::<BTreeSet<_>>();

    let missing = expected.difference(&actual).copied().collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "catalog {kind} array is missing official artifact entries: {}",
            missing.join(", ")
        );
    }

    let unexpected = actual.difference(&expected).copied().collect::<Vec<_>>();
    if !unexpected.is_empty() {
        bail!(
            "catalog {kind} array contains non-discovered official artifact entries: {}",
            unexpected.join(", ")
        );
    }

    Ok(())
}

pub(crate) fn compare_catalogs(actual: &Manifest, expected: &Manifest) -> Result<()> {
    compare_artifact_arrays("services", &actual.services, &expected.services)?;
    compare_artifact_arrays("drivers", &actual.drivers, &expected.drivers)?;
    compare_artifact_arrays("tools", &actual.tools, &expected.tools)?;
    compare_artifact_arrays("simulators", &actual.simulators, &expected.simulators)?;
    compare_asset_arrays(&actual.assets, &expected.assets)?;

    if actual != expected {
        bail!("catalog hand-edit drift for the manifest; run `cargo xtask catalog generate`");
    }

    Ok(())
}

fn compare_artifact_arrays(
    label: &str,
    actual: &[ArtifactEntry],
    expected: &[ArtifactEntry],
) -> Result<()> {
    let actual_by_package = artifact_entries_by_package(label, actual)?;
    let expected_by_package = artifact_entries_by_package(label, expected)?;

    for (package, expected_entry) in &expected_by_package {
        let actual_entry = actual_by_package.get(package).with_context(|| {
            format!("catalog {label} array is missing official artifact entry {package}")
        })?;
        if actual_entry.contracts != expected_entry.contracts {
            bail!("catalog schema_id drift for {package}; regenerate from packaged emit-apis");
        }
    }

    if actual.len() != expected.len() {
        bail!(
            "catalog {label} entry count drift: recorded {}, expected {}",
            actual.len(),
            expected.len()
        );
    }
    Ok(())
}

fn artifact_entries_by_package<'a>(
    label: &str,
    entries: &'a [ArtifactEntry],
) -> Result<BTreeMap<&'a str, &'a ArtifactEntry>> {
    let mut map = BTreeMap::new();
    for entry in entries {
        if map.insert(entry.package.as_str(), entry).is_some() {
            bail!(
                "catalog {label} array contains duplicate package {}",
                entry.package
            );
        }
    }
    Ok(map)
}

fn compare_asset_arrays(actual: &[AssetEntry], expected: &[AssetEntry]) -> Result<()> {
    let mut actual_by_package = BTreeMap::new();
    for entry in actual {
        if actual_by_package
            .insert(entry.package.as_str(), entry)
            .is_some()
        {
            bail!(
                "catalog assets array contains duplicate package {}",
                entry.package
            );
        }
    }
    let mut expected_by_package = BTreeMap::new();
    for entry in expected {
        if expected_by_package
            .insert(entry.package.as_str(), entry)
            .is_some()
        {
            bail!(
                "catalog assets array contains duplicate package {}",
                entry.package
            );
        }
    }

    for package in expected_by_package.keys() {
        if !actual_by_package.contains_key(package) {
            bail!("catalog assets array is missing official artifact entry {package}");
        }
    }

    if actual.len() != expected.len() {
        bail!(
            "catalog assets entry count drift: recorded {}, expected {}",
            actual.len(),
            expected.len()
        );
    }
    Ok(())
}
