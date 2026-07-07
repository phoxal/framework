use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use phoxal::catalog::{
    Artifact as CatalogArtifact, ArtifactEntry, AssetEntry, Channel, Contract, Manifest,
};

use crate::api::sync_features::{ApiGeneration, GenerationChannel, api_generations_from_workspace};
use crate::release::package::{self, EmitApisMetadata};
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
    /// Generate from direct `emit-apis` projections, without release builds or
    /// tarballs. CI uses this as the cheap PR gate; plan #01 release jobs should
    /// use package-output mode so released assets carry real checksums.
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
    let api_generations = api_generations_from_workspace(workspace)?;
    build_catalog_revision_from_artifacts(
        workspace.root(),
        artifacts,
        &api_generations,
        options,
        host_triple,
    )
}

pub(crate) fn build_catalog_revision_from_artifacts(
    root: &Path,
    artifacts: &[OfficialArtifact],
    api_generations: &[ApiGeneration],
    options: &GenerateOptions,
    host_triple: &str,
) -> Result<Manifest> {
    let live_metadata = live_metadata_for_artifacts(root, artifacts, options)?;
    build_manifest_from_live_metadata(
        artifacts,
        api_generations,
        &live_metadata,
        options,
        host_triple,
    )
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

/// Fetches `emit-apis` live (via `cargo run`) for every runtime artifact that
/// is not being carried over from the previous catalog. This is the single
/// source of contract/config/bus-abi metadata regardless of generation mode:
/// there is no packaged sidecar to read anymore (the old per-target
/// `.emit-apis.json` is gone), so the generator always asks the artifact
/// itself, once, on the host.
fn live_metadata_for_artifacts(
    root: &Path,
    artifacts: &[OfficialArtifact],
    options: &GenerateOptions,
) -> Result<BTreeMap<String, EmitApisMetadata>> {
    let mut metadata = BTreeMap::new();
    for artifact in artifacts {
        if artifact.kind == ArtifactKind::ComponentAssets
            || should_reuse_previous(artifact, options)
        {
            continue;
        }
        let stdout = package::emit_apis_from_cargo_run(root, artifact)?;
        let parsed = package::parse_emit_apis_json(&stdout, &artifact.id, artifact.kind)
            .with_context(|| format!("invalid emit-apis metadata from {}", artifact.package))?;
        metadata.insert(artifact.package.clone(), parsed);
    }
    Ok(metadata)
}

fn build_manifest_from_live_metadata(
    artifacts: &[OfficialArtifact],
    api_generations: &[ApiGeneration],
    live_metadata: &BTreeMap<String, EmitApisMetadata>,
    options: &GenerateOptions,
    host_triple: &str,
) -> Result<Manifest> {
    let channels_by_generation = channels_by_generation(api_generations)?;

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

    let mut schemas_by_generation = schemas_by_previous_artifacts(&previous_artifacts)?;
    merge_schemas_by_generation(
        &mut schemas_by_generation,
        &schemas_by_live_metadata(live_metadata)?,
    )?;

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
            &channels_by_generation,
            live_metadata,
            &previous_artifacts,
            api_generations,
            &schemas_by_generation,
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

#[allow(clippy::too_many_arguments)]
fn artifact_entry(
    artifact: &OfficialArtifact,
    channels_by_generation: &BTreeMap<String, Channel>,
    live_metadata: &BTreeMap<String, EmitApisMetadata>,
    previous_artifacts: &BTreeMap<String, &ArtifactEntry>,
    api_generations: &[ApiGeneration],
    schemas_by_generation: &BTreeMap<String, BTreeMap<String, String>>,
    options: &GenerateOptions,
    host_triple: &str,
) -> Result<ArtifactEntry> {
    if should_reuse_previous(artifact, options) {
        return reuse_previous_artifact(artifact, previous_artifacts);
    }

    let metadata = live_metadata
        .get(&artifact.package)
        .with_context(|| format!("missing live emit-apis metadata for {}", artifact.package))?;

    let api_generation = metadata.api_version.clone();
    let channel = channels_by_generation
        .get(&api_generation)
        .with_context(|| {
            format!(
                "{} emitted unknown api_generation '{}'",
                artifact.package, api_generation
            )
        })?;

    let contracts = contracts_from_metadata(metadata, &artifact.package)?;
    let changed = changed_contracts(
        &api_generation,
        &contracts,
        api_generations,
        schemas_by_generation,
    );

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

    let mut channels = BTreeMap::new();
    channels.insert(*channel, artifact.version.clone());

    Ok(ArtifactEntry {
        package: artifact.package.clone(),
        version: artifact.version.clone(),
        api_generation,
        contracts,
        config_schema: Some(metadata.config_schema.clone()),
        bus_abi: metadata.bus_abi.clone(),
        artifacts: artifacts_map,
        channels,
        changed_contracts: changed,
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

fn schemas_by_previous_artifacts(
    previous: &BTreeMap<String, &ArtifactEntry>,
) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    let mut schemas = BTreeMap::<String, BTreeMap<String, String>>::new();
    for entry in previous.values() {
        let generation_schemas = schemas.entry(entry.api_generation.clone()).or_default();
        for contract in &entry.contracts {
            match generation_schemas.get(&contract.family) {
                Some(existing) if existing != &contract.schema_id => bail!(
                    "{} reports schema_id {} for {}, but generation {} already has {}",
                    entry.package,
                    contract.schema_id,
                    contract.family,
                    entry.api_generation,
                    existing
                ),
                Some(_) => {}
                None => {
                    generation_schemas.insert(contract.family.clone(), contract.schema_id.clone());
                }
            }
        }
    }
    Ok(schemas)
}

fn schemas_by_live_metadata(
    live_metadata: &BTreeMap<String, EmitApisMetadata>,
) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    let mut schemas = BTreeMap::<String, BTreeMap<String, String>>::new();
    for (package, metadata) in live_metadata {
        let generation_schemas = schemas.entry(metadata.api_version.clone()).or_default();
        for contract in &metadata.required_contracts {
            let family = contract
                .family
                .as_ref()
                .context("validated emit-apis metadata lost family")?;
            let schema_id = contract
                .schema_id
                .as_ref()
                .context("validated emit-apis metadata lost schema_id")?;
            match generation_schemas.get(family) {
                Some(existing) if existing != schema_id => bail!(
                    "{package} reports schema_id {} for {family}, but generation {} already has {}",
                    schema_id,
                    metadata.api_version,
                    existing
                ),
                Some(_) => {}
                None => {
                    generation_schemas.insert(family.clone(), schema_id.clone());
                }
            }
        }
    }
    Ok(schemas)
}

fn merge_schemas_by_generation(
    target: &mut BTreeMap<String, BTreeMap<String, String>>,
    source: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<()> {
    for (generation, source_schemas) in source {
        let target_schemas = target.entry(generation.clone()).or_default();
        for (family, schema_id) in source_schemas {
            match target_schemas.get(family) {
                Some(existing) if existing != schema_id => bail!(
                    "schema_id {} for {family} conflicts with {} in generation {generation}",
                    schema_id,
                    existing
                ),
                Some(_) => {}
                None => {
                    target_schemas.insert(family.clone(), schema_id.clone());
                }
            }
        }
    }
    Ok(())
}

fn contracts_from_metadata(metadata: &EmitApisMetadata, package: &str) -> Result<Vec<Contract>> {
    let mut contracts = Vec::with_capacity(metadata.required_contracts.len());
    for contract in &metadata.required_contracts {
        contracts.push(Contract {
            family: contract
                .family
                .clone()
                .with_context(|| format!("{package} contract missing family"))?,
            schema_id: contract
                .schema_id
                .clone()
                .with_context(|| format!("{package} contract missing schema_id"))?,
        });
    }
    contracts.sort();
    Ok(contracts)
}

fn changed_contracts(
    api_generation: &str,
    contracts: &[Contract],
    api_generations: &[ApiGeneration],
    schemas_by_generation: &BTreeMap<String, BTreeMap<String, String>>,
) -> Vec<String> {
    let Some(index) = api_generations
        .iter()
        .position(|generation| generation.name == api_generation)
    else {
        return Vec::new();
    };
    if index == 0 {
        return Vec::new();
    }

    let previous_schemas = api_generations[..index]
        .iter()
        .rev()
        .find_map(|generation| schemas_by_generation.get(&generation.name));
    let Some(previous_schemas) = previous_schemas else {
        return contracts
            .iter()
            .map(|contract| contract.family.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    };

    contracts
        .iter()
        .filter(|contract| {
            previous_schemas
                .get(&contract.family)
                .is_none_or(|previous| previous != &contract.schema_id)
        })
        .map(|contract| contract.family.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn channels_by_generation(api_generations: &[ApiGeneration]) -> Result<BTreeMap<String, Channel>> {
    let mut channels = BTreeMap::new();
    for generation in api_generations {
        let channel = match generation.channel {
            GenerationChannel::Stable => Channel::Stable,
            GenerationChannel::Preview => Channel::Preview,
        };
        if channels.insert(generation.name.clone(), channel).is_some() {
            bail!("duplicate API generation {}", generation.name);
        }
    }
    Ok(channels)
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
        let metadata = package::parse_emit_apis_json(
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
            ArtifactKind::Service,
        )?;
        let mut live_metadata = BTreeMap::new();
        live_metadata.insert(artifact.package.clone(), metadata);
        build_manifest_from_live_metadata(
            &[artifact],
            &[ApiGeneration {
                name: "y2026_1".to_string(),
                channel: GenerationChannel::Stable,
            }],
            &live_metadata,
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

    fn generations() -> Vec<ApiGeneration> {
        vec![ApiGeneration {
            name: "y2026_1".to_string(),
            channel: GenerationChannel::Stable,
        }]
    }

    fn emit_json(id: &str, kind: &str, family: &str, schema_id: &str) -> String {
        format!(
            r#"{{
  "schema": "phoxal.emit-apis/v0",
  "artifact": {{ "kind": "{kind}", "id": "{id}" }},
  "framework": {{ "version": "0.21.0" }},
  "api_version": "y2026_1",
  "participant_class": "checked",
  "bus_abi": "phoxal-bus/v0",
  "required_contracts": [
    {{
      "api_version": "y2026_1",
      "schema_id": "{schema_id}",
      "family": "{family}",
      "topic": "fixture/topic",
      "direction": "publish"
    }}
  ],
  "config_schema": {{ "type": "object" }}
}}"#
        )
    }

    /// Writes a fake packaged tarball + checksum (no `emit-apis` sidecar: that
    /// metadata is fetched live by the generator, not read from `package_dir`
    /// - see [`live_metadata_for_artifacts`]).
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
        live_metadata: &BTreeMap<String, EmitApisMetadata>,
        package_dir: &Path,
    ) -> Result<Manifest> {
        build_manifest_from_live_metadata(
            artifacts,
            &generations(),
            live_metadata,
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

    fn live_metadata_for(
        artifact: &OfficialArtifact,
        json: &str,
    ) -> Result<BTreeMap<String, EmitApisMetadata>> {
        let metadata = package::parse_emit_apis_json(json.as_bytes(), &artifact.id, artifact.kind)?;
        let mut map = BTreeMap::new();
        map.insert(artifact.package.clone(), metadata);
        Ok(map)
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
        let live = live_metadata_for(
            &service,
            &emit_json("drive", "service", "drive::Target", "0123456789abcdef"),
        )?;

        let catalog = build_from_outputs(&[service], &live, temp.path())?;
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
        let live = live_metadata_for(
            &service,
            &emit_json("drive", "service", "drive::Target", "0123456789abcdef"),
        )?;

        let catalog = build_from_outputs(&[service], &live, temp.path())?;
        let entry = &catalog.services[0];
        assert_eq!(entry.package, "phoxal/service-drive");
        assert!(entry.artifacts.contains_key("x86_64-unknown-linux-gnu"));
        assert_eq!(entry.channels[&Channel::Stable], "0.1.0");
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
        let live = live_metadata_for(
            &service,
            &emit_json("drive", "service", "drive::Target", "0123456789abcdef"),
        )?;

        let err = build_from_outputs(&[service, driver], &live, temp.path()).unwrap_err();
        assert_error_contains(&err, "missing live emit-apis metadata");
        Ok(())
    }

    #[test]
    fn coverage_gate_fails_on_mismatched_schema_id() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let service = artifact(
            "phoxal-service-drive",
            "drive",
            ArtifactKind::Service,
            "0.1.0",
        );
        write_packaged_fixture(temp.path(), &service, "x86_64-unknown-linux-gnu")?;
        let live = live_metadata_for(
            &service,
            &emit_json("drive", "service", "drive::Target", "0123456789abcdef"),
        )?;

        let expected = build_from_outputs(std::slice::from_ref(&service), &live, temp.path())?;
        let mut edited = expected.clone();
        edited.services[0].contracts[0].schema_id = "1111111111111111".to_string();
        edited = edited.finalize()?;
        let err = compare_catalogs(&edited, &expected).unwrap_err();
        assert_error_contains(&err, "schema_id drift");
        Ok(())
    }

    #[test]
    fn coverage_gate_fails_on_hand_edited_entry() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let service = artifact(
            "phoxal-service-drive",
            "drive",
            ArtifactKind::Service,
            "0.1.0",
        );
        write_packaged_fixture(temp.path(), &service, "x86_64-unknown-linux-gnu")?;
        let live = live_metadata_for(
            &service,
            &emit_json("drive", "service", "drive::Target", "0123456789abcdef"),
        )?;

        let expected = build_from_outputs(std::slice::from_ref(&service), &live, temp.path())?;
        let mut edited = expected.clone();
        edited.services[0].bus_abi = "phoxal-bus/v1".to_string();
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
