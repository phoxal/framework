use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use phoxal::catalog::{
    Artifact as CatalogArtifact, ArtifactEntry, AssetEntry, Channel, Contract, Manifest,
};

use crate::release::metadata::ParticipantMeta;
use crate::release::package;
use crate::release::plan::{ReleasePlan, load_release_plan};
use crate::workspace::{
    ArtifactKind, OfficialArtifact, TARGET_INDEPENDENT_SCOPE, Workspace, require_nonempty_artifacts,
};

const DEFAULT_CATALOG_OUT: &str = "target/xtask/catalog/phoxal-artifacts.json";
const DEFAULT_PACKAGE_DIR: &str = "target/xtask/release";

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long, value_name = "PATH", default_value = DEFAULT_CATALOG_OUT)]
    pub out: PathBuf,
    #[arg(long, value_name = "DIR", default_value = DEFAULT_PACKAGE_DIR)]
    pub package_dir: PathBuf,
    #[arg(long = "target", value_name = "TRIPLE")]
    pub targets: Vec<String>,
    #[arg(long, value_name = "PATH")]
    pub release_plan: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub previous_catalog: Option<PathBuf>,
    /// Generate from a plain host build of each artifact (extracting its
    /// compiled-in `#[derive(phoxal::Api)]` metadata section), without
    /// release builds or tarballs. CI uses this as the cheap PR gate; plan
    /// #01 release jobs should use package-output mode so released assets
    /// carry real checksums.
    #[arg(long)]
    pub metadata_only: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputMode {
    PackageOutputs,
    MetadataOnly,
}

#[derive(Debug)]
pub(crate) struct GenerateOptions {
    pub package_dir: PathBuf,
    pub mode: InputMode,
    pub target_triples: Vec<String>,
    pub release_plan: Option<ReleasePlan>,
    pub previous_catalog: Option<Manifest>,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let artifacts = workspace.official_artifacts();
    require_nonempty_artifacts(artifacts)?;

    let mode = if args.metadata_only {
        InputMode::MetadataOnly
    } else {
        InputMode::PackageOutputs
    };
    let package_dir = package::workspace_relative_out_dir(&workspace, &args.package_dir);
    let out = workspace_relative_path(&workspace, &args.out);
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
        .map(|path| {
            crate::catalog::verify::verify_catalog_path(&workspace_relative_path(&workspace, path))
        })
        .transpose()?;

    if mode == InputMode::PackageOutputs && release_plan.is_none() && previous_catalog.is_none() {
        fs::create_dir_all(&package_dir)
            .with_context(|| format!("failed to create {}", package_dir.display()))?;
        // Crate-backed artifacts build once per requested target; component
        // asset bundles are target-independent and package exactly once.
        let crate_backed = artifacts
            .iter()
            .filter(|artifact| artifact.kind.has_crate())
            .cloned()
            .collect::<Vec<_>>();
        let component_assets = artifacts
            .iter()
            .filter(|artifact| artifact.kind == ArtifactKind::ComponentAssets)
            .cloned()
            .collect::<Vec<_>>();
        for target_triple in &target_triples {
            package::package_artifacts(
                &workspace,
                &crate_backed,
                &package_dir,
                &host_triple,
                target_triple,
            )?;
        }
        package::package_artifacts(
            &workspace,
            &component_assets,
            &package_dir,
            &host_triple,
            TARGET_INDEPENDENT_SCOPE,
        )?;
    }

    let manifest = build_catalog_revision(
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
    write_catalog(&out, &manifest)?;
    println!(
        "generated catalog {} with {} entries at {}",
        manifest.revision,
        manifest.total_entries(),
        out.display()
    );
    Ok(())
}

pub(crate) fn default_catalog_path(workspace: &Workspace) -> PathBuf {
    workspace.root().join(DEFAULT_CATALOG_OUT)
}

pub(crate) fn workspace_relative_path(workspace: &Workspace, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.root().join(path)
    }
}

pub(crate) fn write_catalog(path: &Path, manifest: &Manifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut json =
        serde_json::to_string_pretty(manifest).context("failed to serialize catalog manifest")?;
    json.push('\n');
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn build_catalog_revision(
    workspace: &Workspace,
    options: &GenerateOptions,
    host_triple: &str,
) -> Result<Manifest> {
    let artifacts = workspace.official_artifacts();
    require_nonempty_artifacts(artifacts)?;
    build_catalog_revision_from_artifacts(workspace, artifacts, options, host_triple)
}

pub(crate) fn build_catalog_revision_from_artifacts(
    workspace: &Workspace,
    artifacts: &[OfficialArtifact],
    options: &GenerateOptions,
    host_triple: &str,
) -> Result<Manifest> {
    let metadata = extracted_metadata_for_artifacts(workspace, artifacts, options, host_triple)?;
    build_manifest_from_metadata(artifacts, &metadata, options, host_triple)
}

/// Whether `artifact` is unaffected by the current release plan and should be
/// carried over verbatim from `options.previous_catalog` rather than
/// recomputed: preserves the immutability of entries outside an incremental
/// release rather than letting incidental local state (a stray `package_dir`
/// entry, a rebuilt binary) drift them.
fn should_reuse_previous(artifact: &OfficialArtifact, options: &GenerateOptions) -> bool {
    options.mode == InputMode::PackageOutputs
        && options.previous_catalog.is_some()
        && options.release_plan.as_ref().is_some_and(|plan| {
            !plan
                .artifacts
                .iter()
                .any(|item| item.package == artifact.package)
        })
}

/// Extracts the compiled-in `#[derive(phoxal::Api)]` metadata for every
/// runtime artifact that is not being carried over from the previous catalog,
/// reading the section straight off an object file (never running the binary).
///
/// Where the object file comes from is mode-dependent, and this is the
/// difference that makes the metadata authoritative in the release pipeline:
///
/// - **`PackageOutputs`** (release / `catalog-publish`, and the local full
///   `catalog generate`): read the ACTUAL released binary out of its
///   `{stem}.tar.zst` in `package_dir` - the exact cross-compiled bytes being
///   shipped, parsed format/arch-agnostically on whatever host runs the job.
///   The catalog's `contracts[]` are therefore inseparable from the artifact.
/// - **`MetadataOnly`** (the cheap CI PR gate): no tarball exists yet, so do a
///   plain host `cargo build` purely to materialize the linker section, then
///   extract from that. Valid because the section is target-independent (no
///   `Api` is target-conditional today).
fn extracted_metadata_for_artifacts(
    workspace: &Workspace,
    artifacts: &[OfficialArtifact],
    options: &GenerateOptions,
    host_triple: &str,
) -> Result<BTreeMap<String, ParticipantMeta>> {
    let mut metadata = BTreeMap::new();
    for artifact in artifacts {
        if artifact.kind == ArtifactKind::ComponentAssets
            || should_reuse_previous(artifact, options)
        {
            continue;
        }
        let meta = match options.mode {
            InputMode::PackageOutputs => {
                let target_triples = target_triples_for_artifact(artifact, options, host_triple)?;
                package::extract_metadata_from_packaged(
                    artifact,
                    &options.package_dir,
                    &target_triples,
                )?
            }
            InputMode::MetadataOnly => package::build_and_extract_metadata(workspace, artifact)
                .with_context(|| {
                    format!("failed to extract API metadata for {}", artifact.package)
                })?,
        };
        metadata.insert(artifact.package.clone(), meta);
    }
    Ok(metadata)
}

fn build_manifest_from_metadata(
    artifacts: &[OfficialArtifact],
    metadata: &BTreeMap<String, ParticipantMeta>,
    options: &GenerateOptions,
    host_triple: &str,
) -> Result<Manifest> {
    let previous_assets = options
        .previous_catalog
        .as_ref()
        .map(previous_assets_by_package)
        .transpose()?
        .unwrap_or_default();
    let previous_artifacts = options
        .previous_catalog
        .as_ref()
        .map(previous_artifacts_by_package)
        .transpose()?
        .unwrap_or_default();

    let mut assets = Vec::new();
    let mut services = Vec::new();
    let mut drivers = Vec::new();
    let mut tools = Vec::new();
    let mut simulators = Vec::new();

    for artifact in artifacts {
        if artifact.kind == ArtifactKind::ComponentAssets {
            assets.push(asset_entry(artifact, options, &previous_assets)?);
            continue;
        }

        let entry = artifact_entry(
            artifact,
            metadata,
            &previous_artifacts,
            options,
            host_triple,
        )?;

        match artifact.kind {
            ArtifactKind::Service => services.push(entry),
            ArtifactKind::ComponentDriver => drivers.push(entry),
            ArtifactKind::Tool => tools.push(entry),
            ArtifactKind::Simulator => simulators.push(entry),
            ArtifactKind::ComponentAssets => unreachable!("component assets handled above"),
        }
    }

    for entries in [&mut services, &mut drivers, &mut tools, &mut simulators] {
        entries.sort_by(|left, right| left.package.cmp(&right.package));
    }
    assets.sort_by(|left, right| left.package.cmp(&right.package));

    Manifest::new(assets, services, drivers, tools, simulators).finalize()
}

fn asset_entry(
    artifact: &OfficialArtifact,
    options: &GenerateOptions,
    previous_assets: &BTreeMap<String, &AssetEntry>,
) -> Result<AssetEntry> {
    if should_reuse_previous(artifact, options) {
        return reuse_previous_asset(artifact, previous_assets);
    }

    let mut asset_artifacts = BTreeMap::new();
    if options.mode == InputMode::PackageOutputs {
        let output = package::read_packaged_output(
            artifact,
            &options.package_dir,
            TARGET_INDEPENDENT_SCOPE,
        )?;
        asset_artifacts.insert(
            TARGET_INDEPENDENT_SCOPE.to_string(),
            CatalogArtifact {
                tarball: output.tarball_name,
                sha256: output.tarball_sha256,
            },
        );
    }

    // Component assets are not gated by an API generation cycle (docs #21):
    // there is no preview/stable split for a mesh bundle, so it always
    // publishes on the stable channel at its own version.
    let mut channels = BTreeMap::new();
    channels.insert(Channel::Stable, artifact.version.clone());

    Ok(AssetEntry {
        package: artifact.package.clone(),
        version: artifact.version.clone(),
        artifacts: asset_artifacts,
        channels,
    })
}

fn artifact_entry(
    artifact: &OfficialArtifact,
    metadata: &BTreeMap<String, ParticipantMeta>,
    previous_artifacts: &BTreeMap<String, &ArtifactEntry>,
    options: &GenerateOptions,
    host_triple: &str,
) -> Result<ArtifactEntry> {
    if should_reuse_previous(artifact, options) {
        return reuse_previous_artifact(artifact, previous_artifacts);
    }

    let meta = metadata
        .get(&artifact.package)
        .with_context(|| format!("missing extracted API metadata for {}", artifact.package))?;
    let contracts = contracts_from_metadata(meta);

    let mut artifacts_map = BTreeMap::new();
    if options.mode == InputMode::PackageOutputs {
        let target_triples = target_triples_for_artifact(artifact, options, host_triple)?;
        for triple in &target_triples {
            let output = package::read_packaged_output(artifact, &options.package_dir, triple)?;
            artifacts_map.insert(
                triple.clone(),
                CatalogArtifact {
                    tarball: output.tarball_name,
                    sha256: output.tarball_sha256,
                },
            );
        }
    }

    // No per-generation channel routing (X-tools slice): a participant's `Api`
    // struct may mix contracts from several API generations freely, so there
    // is no single generation left to key a preview/stable split off. Every
    // artifact publishes on the stable channel until a coherence-driven
    // `heads` mechanism replaces this (organization/tmp/ci-release-refactor).
    let mut channels = BTreeMap::new();
    channels.insert(Channel::Stable, artifact.version.clone());

    Ok(ArtifactEntry {
        package: artifact.package.clone(),
        version: artifact.version.clone(),
        contracts,
        // Placeholder until the config JSON Schema gets a host-side `build.rs`
        // materialization step (RECONCILIATION correction #12) - do not try to
        // reproduce `schemars::schema_for!` here.
        config_schema: Some(serde_json::json!({})),
        artifacts: artifacts_map,
        channels,
        changed_contracts: Vec::new(),
    })
}

fn reuse_previous_asset(
    artifact: &OfficialArtifact,
    previous: &BTreeMap<String, &AssetEntry>,
) -> Result<AssetEntry> {
    previous
        .get(&artifact.package)
        .filter(|entry| entry.version == artifact.version)
        .map(|entry| (*entry).clone())
        .with_context(|| format!("missing projection for {}", artifact.package))
}

fn reuse_previous_artifact(
    artifact: &OfficialArtifact,
    previous: &BTreeMap<String, &ArtifactEntry>,
) -> Result<ArtifactEntry> {
    previous
        .get(&artifact.package)
        .filter(|entry| entry.version == artifact.version)
        .map(|entry| (*entry).clone())
        .with_context(|| format!("missing projection for {}", artifact.package))
}

fn previous_assets_by_package(manifest: &Manifest) -> Result<BTreeMap<String, &AssetEntry>> {
    let mut entries = BTreeMap::new();
    for entry in &manifest.assets {
        if entries.insert(entry.package.clone(), entry).is_some() {
            bail!(
                "previous catalog contains duplicate package {}",
                entry.package
            );
        }
    }
    Ok(entries)
}

fn previous_artifacts_by_package(manifest: &Manifest) -> Result<BTreeMap<String, &ArtifactEntry>> {
    let mut entries = BTreeMap::new();
    for entry in manifest.artifact_entries() {
        if entries.insert(entry.package.clone(), entry).is_some() {
            bail!(
                "previous catalog contains duplicate package {}",
                entry.package
            );
        }
    }
    Ok(entries)
}

/// Projects an extracted `#[derive(phoxal::Api)]` manifest into the catalog's
/// `Contract` list: version-qualified name + role, deduplicated (a `Server<Req,
/// Resp>` field contributes two entries that may collide with another field's
/// entries in the same role).
fn contracts_from_metadata(meta: &ParticipantMeta) -> Vec<Contract> {
    let mut contracts: Vec<Contract> = meta
        .contracts
        .iter()
        .map(|entry| Contract {
            family: entry.contract.clone(),
            role: entry.role.clone(),
        })
        .collect();
    contracts.sort();
    contracts.dedup();
    contracts
}

fn target_triples_for_artifact(
    artifact: &OfficialArtifact,
    options: &GenerateOptions,
    host_triple: &str,
) -> Result<Vec<String>> {
    // `component_assets` bundles are always the single target-independent scope
    // (docs #21): never a real triple, never subject to the release-plan/host
    // target selection that follows.
    if artifact.kind == ArtifactKind::ComponentAssets {
        return Ok(vec![TARGET_INDEPENDENT_SCOPE.to_string()]);
    }

    if let Some(plan) = &options.release_plan {
        if let Some(planned) = plan
            .artifacts
            .iter()
            .find(|planned| planned.package == artifact.package)
        {
            return Ok(planned.target_triples.clone());
        }
    }

    let targets = if options.target_triples.is_empty() {
        vec![host_triple.to_string()]
    } else {
        options.target_triples.clone()
    };
    if options.mode == InputMode::MetadataOnly {
        return Ok(targets);
    }
    let mut supported = Vec::new();
    for target in targets {
        if artifact.supports_target(&target) {
            supported.push(target);
        }
    }
    if supported.is_empty() {
        bail!(
            "{} has no supported targets in requested target set",
            artifact.package
        );
    }
    supported.sort();
    supported.dedup();
    Ok(supported)
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use crate::release::metadata::ParticipantMetaContract;

    pub(crate) fn fixture_meta() -> ParticipantMeta {
        ParticipantMeta {
            participant_api: "Api".to_string(),
            contracts: vec![ParticipantMetaContract {
                field: "target".to_string(),
                role: "publish".to_string(),
                contract: "y2026_1::drive::Target".to_string(),
            }],
        }
    }

    pub(crate) fn fixture_catalog() -> Result<Manifest> {
        let artifact = OfficialArtifact {
            package: "phoxal/service-drive".to_string(),
            package_name: Some("phoxal-service-drive".to_string()),
            kind: ArtifactKind::Service,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::new(),
            bin_name: Some("phoxal-service-drive".to_string()),
            id: "drive".to_string(),
            metadata: Default::default(),
        };
        let mut metadata = BTreeMap::new();
        metadata.insert(artifact.package.clone(), fixture_meta());
        build_manifest_from_metadata(
            &[artifact],
            &metadata,
            &GenerateOptions {
                package_dir: PathBuf::new(),
                mode: InputMode::MetadataOnly,
                target_triples: vec!["x86_64-unknown-linux-gnu".to_string()],
                release_plan: None,
                previous_catalog: None,
            },
            "x86_64-unknown-linux-gnu",
        )
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::catalog::check::compare_catalogs;
    use crate::release::metadata::ParticipantMetaContract;

    fn artifact(
        package_name: &str,
        id: &str,
        kind: ArtifactKind,
        version: &str,
    ) -> OfficialArtifact {
        OfficialArtifact {
            package: crate::workspace::package_identity(kind, id),
            package_name: Some(package_name.to_string()),
            kind,
            version: version.to_string(),
            crate_dir: PathBuf::new(),
            bin_name: Some(package_name.to_string()),
            id: id.to_string(),
            metadata: Default::default(),
        }
    }

    fn component_assets_artifact(id: &str, version: &str) -> OfficialArtifact {
        OfficialArtifact {
            package: crate::workspace::package_identity(ArtifactKind::ComponentAssets, id),
            package_name: None,
            kind: ArtifactKind::ComponentAssets,
            version: version.to_string(),
            crate_dir: PathBuf::new(),
            bin_name: None,
            id: id.to_string(),
            metadata: Default::default(),
        }
    }

    fn meta(family: &str, role: &str) -> ParticipantMeta {
        ParticipantMeta {
            participant_api: "Api".to_string(),
            contracts: vec![ParticipantMetaContract {
                field: "handle".to_string(),
                role: role.to_string(),
                contract: family.to_string(),
            }],
        }
    }

    /// Writes a fake packaged tarball + checksum (no metadata sidecar: the
    /// generator extracts contracts from a plain host build, not from
    /// `package_dir` - see [`extracted_metadata_for_artifacts`]).
    fn write_packaged_fixture(dir: &Path, artifact: &OfficialArtifact, triple: &str) -> Result<()> {
        fs::create_dir_all(dir)?;
        let stem = package::asset_stem(artifact, triple);
        let asset = dir.join(format!("{stem}.tar.zst"));
        let checksum = dir.join(format!("{stem}.tar.zst.sha256"));
        fs::write(&asset, b"fake tarball")?;
        let digest = hex::encode(Sha256::digest(b"fake tarball"));
        let mut file = File::create(checksum)?;
        writeln!(
            file,
            "{digest}  {}",
            asset.file_name().unwrap().to_str().unwrap()
        )?;
        Ok(())
    }

    fn build_from_outputs(
        artifacts: &[OfficialArtifact],
        metadata: &BTreeMap<String, ParticipantMeta>,
        package_dir: &Path,
    ) -> Result<Manifest> {
        build_manifest_from_metadata(
            artifacts,
            metadata,
            &GenerateOptions {
                package_dir: package_dir.to_path_buf(),
                mode: InputMode::PackageOutputs,
                target_triples: vec!["x86_64-unknown-linux-gnu".to_string()],
                release_plan: None,
                previous_catalog: None,
            },
            "x86_64-unknown-linux-gnu",
        )
    }

    #[test]
    fn schema_round_trip_preserves_checksum() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let service = artifact(
            "phoxal-service-drive",
            "drive",
            ArtifactKind::Service,
            "0.1.0",
        );
        write_packaged_fixture(temp.path(), &service, "x86_64-unknown-linux-gnu")?;
        let mut metadata = BTreeMap::new();
        metadata.insert(service.package.clone(), meta("drive::Target", "publish"));

        let catalog = build_from_outputs(&[service], &metadata, temp.path())?;
        catalog.verify()?;
        let json = serde_json::to_string_pretty(&catalog)?;
        let reparsed: Manifest = serde_json::from_str(&json)?;
        assert_eq!(catalog, reparsed);
        reparsed.verify()?;
        Ok(())
    }

    #[test]
    fn generation_from_package_output_fixture_records_released_asset() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let service = artifact(
            "phoxal-service-drive",
            "drive",
            ArtifactKind::Service,
            "0.1.0",
        );
        write_packaged_fixture(temp.path(), &service, "x86_64-unknown-linux-gnu")?;
        let mut metadata = BTreeMap::new();
        metadata.insert(service.package.clone(), meta("drive::Target", "publish"));

        let catalog = build_from_outputs(&[service], &metadata, temp.path())?;
        let entry = &catalog.services[0];
        assert_eq!(entry.package, "phoxal/service-drive");
        assert!(entry.artifacts.contains_key("x86_64-unknown-linux-gnu"));
        assert_eq!(entry.channels[&Channel::Stable], "0.1.0");
        assert_eq!(entry.contracts.len(), 1);
        assert_eq!(entry.contracts[0].family, "drive::Target");
        assert_eq!(entry.contracts[0].role, "publish");
        Ok(())
    }

    #[test]
    fn generation_from_package_output_fixture_records_component_assets() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let assets = component_assets_artifact("ddsm115", "0.1.0");
        write_packaged_fixture(temp.path(), &assets, TARGET_INDEPENDENT_SCOPE)?;

        let catalog = build_from_outputs(&[assets], &BTreeMap::new(), temp.path())?;

        let entry = &catalog.assets[0];
        assert_eq!(entry.package, "phoxal/component-ddsm115-assets");
        assert!(entry.artifacts.contains_key(TARGET_INDEPENDENT_SCOPE));
        assert_eq!(entry.channels[&Channel::Stable], "0.1.0");
        Ok(())
    }

    #[test]
    fn package_output_mode_requires_component_assets_output() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let assets = component_assets_artifact("ddsm115", "0.1.0");

        let err = build_from_outputs(&[assets], &BTreeMap::new(), temp.path()).unwrap_err();

        assert_error_contains(&err, "missing packaged output");
        Ok(())
    }

    #[test]
    fn coverage_gate_fails_when_artifact_is_missing() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let service = artifact(
            "phoxal-service-drive",
            "drive",
            ArtifactKind::Service,
            "0.1.0",
        );
        let driver = artifact(
            "phoxal-component-ddsm115-driver",
            "ddsm115",
            ArtifactKind::ComponentDriver,
            "0.1.0",
        );
        write_packaged_fixture(temp.path(), &service, "x86_64-unknown-linux-gnu")?;
        let mut metadata = BTreeMap::new();
        metadata.insert(service.package.clone(), meta("drive::Target", "publish"));

        let err = build_from_outputs(&[service, driver], &metadata, temp.path()).unwrap_err();
        assert_error_contains(&err, "missing extracted API metadata");
        Ok(())
    }

    #[test]
    fn coverage_gate_fails_on_hand_edited_contracts() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let service = artifact(
            "phoxal-service-drive",
            "drive",
            ArtifactKind::Service,
            "0.1.0",
        );
        write_packaged_fixture(temp.path(), &service, "x86_64-unknown-linux-gnu")?;
        let mut metadata = BTreeMap::new();
        metadata.insert(service.package.clone(), meta("drive::Target", "publish"));

        let expected = build_from_outputs(std::slice::from_ref(&service), &metadata, temp.path())?;
        let mut edited = expected.clone();
        edited.services[0].contracts[0].family = "drive::OtherTarget".to_string();
        edited = edited.finalize()?;
        let err = compare_catalogs(&edited, &expected).unwrap_err();
        assert_error_contains(&err, "contracts drift");
        Ok(())
    }

    #[test]
    fn coverage_gate_fails_on_hand_edited_channel() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let service = artifact(
            "phoxal-service-drive",
            "drive",
            ArtifactKind::Service,
            "0.1.0",
        );
        write_packaged_fixture(temp.path(), &service, "x86_64-unknown-linux-gnu")?;
        let mut metadata = BTreeMap::new();
        metadata.insert(service.package.clone(), meta("drive::Target", "publish"));

        let expected = build_from_outputs(std::slice::from_ref(&service), &metadata, temp.path())?;
        let mut edited = expected.clone();
        edited.services[0]
            .channels
            .insert(Channel::Preview, "9.9.9".to_string());
        edited = edited.finalize()?;
        let err = compare_catalogs(&edited, &expected).unwrap_err();
        assert_error_contains(&err, "hand-edit drift");
        Ok(())
    }

    fn assert_error_contains(error: &anyhow::Error, needle: &str) {
        assert!(
            error
                .chain()
                .any(|cause| cause.to_string().contains(needle)),
            "expected error chain to contain {needle:?}, got {error:?}"
        );
    }
}
