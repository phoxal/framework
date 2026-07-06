use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use crate::api::sync_features::{ApiGeneration, GenerationChannel, api_generations_from_workspace};
use crate::catalog::schema::{
    ArtifactStatus, CatalogEntry, CatalogProvenance, CatalogRevision, Channel, ContractUse,
    EngineVersions, LaunchFacts, ReleaseAsset, ReleaseAssetMetadata, RouterFacts, SystemdFacts,
};
use crate::release::package::{self, EmitApisMetadata, PackagedOutput};
use crate::release::plan::{ReleasePlan, load_release_plan};
use crate::workspace::{
    ArtifactKind, OfficialArtifact, TARGET_INDEPENDENT_SCOPE, Workspace, require_nonempty_artifacts,
};

const DEFAULT_CATALOG_OUT: &str = "target/xtask/catalog/phoxal-artifact-catalog.json";
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
    pub previous_catalog: Option<CatalogRevision>,
}

#[derive(Debug)]
struct ArtifactProjection {
    metadata: ProjectionMetadata,
    releases: BTreeMap<String, ReleaseProjection>,
    status: BTreeMap<String, ArtifactStatus>,
}

/// The fields a catalog entry needs from a package's projection, sourced either
/// from a real binary's `emit-apis` output (services/tools/simulators/component
/// drivers) or synthesized for a `ComponentAssets` bundle, which has no runtime
/// binary at all (docs #21: assets carry no contracts and are target-independent).
#[derive(Debug)]
enum ProjectionMetadata {
    EmitApis(EmitApisMetadata),
    ComponentAssets { api_generation: String },
}

impl ProjectionMetadata {
    fn api_generation(&self) -> &str {
        match self {
            ProjectionMetadata::EmitApis(metadata) => &metadata.api_version,
            ProjectionMetadata::ComponentAssets { api_generation } => api_generation,
        }
    }

    fn participant_kind(&self, kind: ArtifactKind) -> String {
        match self {
            ProjectionMetadata::EmitApis(metadata) => metadata.artifact.kind.clone(),
            ProjectionMetadata::ComponentAssets { .. } => kind.emit_apis_kind().to_string(),
        }
    }

    fn participant_class(&self) -> Result<String> {
        match self {
            ProjectionMetadata::EmitApis(metadata) => metadata
                .participant_class
                .clone()
                .context("validated emit-apis metadata lost participant_class"),
            // Component assets are files, not a launched participant: they are
            // never "checked" or "privileged" bus participants.
            ProjectionMetadata::ComponentAssets { .. } => Ok("none".to_string()),
        }
    }

    fn framework_version(&self) -> Option<String> {
        match self {
            ProjectionMetadata::EmitApis(metadata) => Some(metadata.framework.version.clone()),
            ProjectionMetadata::ComponentAssets { .. } => None,
        }
    }

    fn required_contracts(&self) -> &[crate::release::package::Contract] {
        match self {
            ProjectionMetadata::EmitApis(metadata) => &metadata.required_contracts,
            ProjectionMetadata::ComponentAssets { .. } => &[],
        }
    }
}

/// `metadata_filename`/`metadata_sha256` are `None` for a
/// [`ArtifactKind::ComponentAssets`] bundle, which carries no `emit-apis`
/// sidecar (docs #21).
#[derive(Debug)]
struct ReleaseProjection {
    asset_filename: String,
    asset_sha256: String,
    metadata_filename: Option<String>,
    metadata_sha256: Option<String>,
    checksum_filename: String,
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
        // `ComponentAssets` packages have no Cargo crate/binary to build; only
        // crate-backed artifacts go through `cargo auditable build` + tarball
        // packaging here.
        let crate_backed = artifacts
            .iter()
            .filter(|artifact| artifact.kind.has_crate())
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
    }

    let revision = build_catalog_revision(
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
    write_catalog(&out, &revision)?;
    println!(
        "generated catalog {} with {} entries at {}",
        revision.revision,
        revision.entries.len(),
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

pub(crate) fn write_catalog(path: &Path, revision: &CatalogRevision) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut json =
        serde_json::to_string_pretty(revision).context("failed to serialize catalog revision")?;
    json.push('\n');
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn build_catalog_revision(
    workspace: &Workspace,
    options: &GenerateOptions,
    host_triple: &str,
) -> Result<CatalogRevision> {
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
) -> Result<CatalogRevision> {
    let projections = projections(root, artifacts, api_generations, options, host_triple)?;
    build_revision_from_projections(
        artifacts,
        api_generations,
        &projections,
        options,
        host_triple,
    )
}

fn projections(
    root: &Path,
    artifacts: &[OfficialArtifact],
    api_generations: &[ApiGeneration],
    options: &GenerateOptions,
    host_triple: &str,
) -> Result<BTreeMap<String, ArtifactProjection>> {
    let mut projections = BTreeMap::new();
    for artifact in artifacts {
        if options.mode == InputMode::PackageOutputs
            && options.previous_catalog.is_some()
            && options.release_plan.as_ref().is_some_and(|plan| {
                !plan
                    .artifacts
                    .iter()
                    .any(|item| item.package == artifact.package)
            })
        {
            continue;
        }
        let projection = if artifact.kind == ArtifactKind::ComponentAssets {
            projection_from_component_assets(artifact, api_generations, options)?
        } else {
            match options.mode {
                InputMode::PackageOutputs => {
                    let target_triples =
                        target_triples_for_artifact(artifact, options, host_triple)?;
                    projection_from_package_outputs(
                        artifact,
                        &options.package_dir,
                        &target_triples,
                    )?
                }
                InputMode::MetadataOnly => projection_from_emit_apis(root, artifact)?,
            }
        };
        projections.insert(artifact.package.clone(), projection);
    }
    Ok(projections)
}

/// `ComponentAssets` bundles have no runtime binary to run `emit-apis` on, so
/// there is nothing to execute; they are synthesized as target-independent,
/// contract-free entries pinned to the latest stable API generation (docs #21:
/// assets carry no contracts and are not per-architecture binaries).
///
/// In [`InputMode::PackageOutputs`], if `cargo xtask release package` already
/// wrote a tarball for this bundle under `options.package_dir`, this reads it
/// and records a real `Released` release asset (`metadata: None` - assets carry
/// no `emit-apis` sidecar). If no packaged tarball is present (e.g. a partial
/// package-outputs run that only packaged some artifacts), the bundle is left
/// `Pending` with no release asset, same as `InputMode::MetadataOnly`.
fn projection_from_component_assets(
    artifact: &OfficialArtifact,
    api_generations: &[ApiGeneration],
    options: &GenerateOptions,
) -> Result<ArtifactProjection> {
    let latest_stable = api_generations
        .iter()
        .rev()
        .find(|generation| generation.channel == GenerationChannel::Stable)
        .with_context(
            || "no stable API generation is declared to pin component_assets packages to",
        )?;
    let metadata = ProjectionMetadata::ComponentAssets {
        api_generation: latest_stable.name.clone(),
    };

    if options.mode == InputMode::PackageOutputs {
        let stem = package::asset_stem(artifact, crate::workspace::TARGET_INDEPENDENT_SCOPE);
        let tarball = options.package_dir.join(format!("{stem}.tar.zst"));
        let checksum = options.package_dir.join(format!("{stem}.tar.zst.sha256"));
        if tarball.is_file() && checksum.is_file() {
            let output = package::read_packaged_output(
                artifact,
                &options.package_dir,
                crate::workspace::TARGET_INDEPENDENT_SCOPE,
            )?;
            let mut releases = BTreeMap::new();
            let mut status = BTreeMap::new();
            releases.insert(
                crate::workspace::TARGET_INDEPENDENT_SCOPE.to_string(),
                release_projection(&output),
            );
            status.insert(
                crate::workspace::TARGET_INDEPENDENT_SCOPE.to_string(),
                ArtifactStatus::Released,
            );
            return Ok(ArtifactProjection {
                metadata,
                releases,
                status,
            });
        }
    }

    Ok(ArtifactProjection {
        metadata,
        releases: BTreeMap::new(),
        // Left empty: `build_revision_from_projections` fills the
        // target-independent scope in with `Pending` below.
        status: BTreeMap::new(),
    })
}

fn projection_from_package_outputs(
    artifact: &OfficialArtifact,
    package_dir: &Path,
    target_triples: &[String],
) -> Result<ArtifactProjection> {
    let first_target = target_triples
        .first()
        .with_context(|| format!("{} has no target triples", artifact.package))?;
    let first = package::read_packaged_output(artifact, package_dir, first_target)?;
    let mut releases = BTreeMap::new();
    let mut status = BTreeMap::new();
    releases.insert(first_target.clone(), release_projection(&first));
    status.insert(first_target.clone(), ArtifactStatus::Released);

    for target in &target_triples[1..] {
        let output = package::read_packaged_output(artifact, package_dir, target)?;
        if output.emit_apis != first.emit_apis {
            bail!(
                "{} emit-apis metadata differs between {} and {}; cross-target metadata must be target-independent",
                artifact.package,
                first_target,
                target
            );
        }
        releases.insert(target.clone(), release_projection(&output));
        status.insert(target.clone(), ArtifactStatus::Released);
    }

    let emit_apis = first.emit_apis.with_context(|| {
        format!(
            "{} is a crate-backed artifact and must have emit-apis metadata",
            artifact.package
        )
    })?;
    Ok(ArtifactProjection {
        metadata: ProjectionMetadata::EmitApis(emit_apis),
        releases,
        status,
    })
}

fn projection_from_emit_apis(
    root: &Path,
    artifact: &OfficialArtifact,
) -> Result<ArtifactProjection> {
    let stdout = package::emit_apis_from_cargo_run(root, artifact)?;
    let parsed = package::parse_emit_apis_json(&stdout, &artifact.id, artifact.kind)
        .with_context(|| format!("invalid emit-apis metadata from {}", artifact.package))?;
    Ok(ArtifactProjection {
        metadata: ProjectionMetadata::EmitApis(parsed),
        releases: BTreeMap::new(),
        status: BTreeMap::new(),
    })
}

fn build_revision_from_projections(
    artifacts: &[OfficialArtifact],
    api_generations: &[ApiGeneration],
    projections: &BTreeMap<String, ArtifactProjection>,
    options: &GenerateOptions,
    host_triple: &str,
) -> Result<CatalogRevision> {
    let channels_by_generation = channels_by_generation(api_generations)?;
    let mut schemas_by_generation = options
        .previous_catalog
        .as_ref()
        .map(|catalog| schemas_by_catalog_entries(&catalog.entries))
        .transpose()?
        .unwrap_or_default();
    merge_schemas_by_generation(
        &mut schemas_by_generation,
        &schemas_by_generation_from_projections(projections)?,
    )?;
    let previous_entries = options
        .previous_catalog
        .as_ref()
        .map(previous_entries_by_package)
        .transpose()?;
    let mut entries = Vec::with_capacity(artifacts.len());

    for artifact in artifacts {
        let Some(projection) = projections.get(&artifact.package) else {
            if let Some(previous_entries) = &previous_entries {
                if let Some(previous) = previous_entries.get(&artifact.package) {
                    if previous.version == artifact.version {
                        entries.push((*previous).clone());
                        continue;
                    }
                }
            }
            bail!("missing projection for {}", artifact.package);
        };
        let metadata = &projection.metadata;
        let api_generation = metadata.api_generation().to_string();
        let channel = channels_by_generation
            .get(&api_generation)
            .with_context(|| {
                format!(
                    "{} emitted unknown api_generation '{}'",
                    artifact.package, api_generation
                )
            })?;
        let contract_uses = contract_uses(metadata, &artifact.package)?;
        let changed_contracts = changed_contracts(
            &api_generation,
            &contract_uses,
            api_generations,
            &schemas_by_generation,
        );

        let target_triples = target_triples_for_artifact(artifact, options, host_triple)?;
        let mut release_assets = BTreeMap::new();
        for (target, release) in &projection.releases {
            // `ComponentAssets` bundles carry no `emit-apis` sidecar (docs #21):
            // `metadata_filename`/`metadata_sha256` are `None` for them, so the
            // catalog's `metadata` is `None` too. Every other kind always
            // packages `Some(..)`.
            let metadata = match (&release.metadata_filename, &release.metadata_sha256) {
                (Some(emit_apis), Some(emit_apis_sha256)) => Some(ReleaseAssetMetadata {
                    emit_apis: emit_apis.clone(),
                    emit_apis_sha256: emit_apis_sha256.clone(),
                    sha256_file: release.checksum_filename.clone(),
                }),
                _ => None,
            };
            release_assets.insert(
                target.clone(),
                ReleaseAsset {
                    asset: release.asset_filename.clone(),
                    sha256: release.asset_sha256.clone(),
                    metadata,
                },
            );
        }

        let mut status = projection.status.clone();
        if status.is_empty() {
            for target in &target_triples {
                status.insert(target.clone(), ArtifactStatus::Pending);
            }
        }

        let mut channels = BTreeMap::new();
        channels.insert(*channel, artifact.version.clone());

        entries.push(CatalogEntry {
            package: artifact.package.clone(),
            kind: artifact.kind,
            version: artifact.version.clone(),
            api_generation,
            contract_uses,
            target_triples,
            release_assets,
            launch_facts: LaunchFacts {
                participant_kind: metadata.participant_kind(artifact.kind),
                participant_class: metadata.participant_class()?,
                router: RouterFacts {
                    needs_zenoh_router: artifact.kind != ArtifactKind::ComponentAssets,
                },
                systemd: SystemdFacts {
                    groups: Vec::new(),
                    devices: Vec::new(),
                    source: "not-modeled-until-native-release-plan-01".to_string(),
                },
            },
            engine_versions: EngineVersions {
                phoxal: metadata.framework_version(),
                phoxal_bus: None,
                zenoh: None,
            },
            channels,
            status,
            changed_contracts,
        });
    }

    entries.sort_by(|left, right| left.package.cmp(&right.package));

    CatalogRevision::new(provenance(options.mode), entries).finalize()
}

fn provenance(mode: InputMode) -> CatalogProvenance {
    match mode {
        InputMode::PackageOutputs => CatalogProvenance {
            generator: "cargo xtask catalog generate".to_string(),
            official_set: "cargo_metadata workspace official artifact discovery".to_string(),
            emit_apis: "cargo xtask release package *.emit-apis.json outputs".to_string(),
            release_assets: "local host-triple cargo xtask release package outputs".to_string(),
            plan_01: "plan-01 fills CI-built per-triple assets, signatures, immutable publication refs, and embedded phoxal-bus/zenoh versions".to_string(),
        },
        InputMode::MetadataOnly => CatalogProvenance {
            generator: "cargo xtask catalog generate --metadata-only".to_string(),
            official_set: "cargo_metadata workspace official artifact discovery".to_string(),
            emit_apis: "direct cargo run -p <artifact> -- emit-apis projections".to_string(),
            release_assets: "none; host target is marked pending in metadata-only mode".to_string(),
            plan_01: "plan-01 fills CI-built per-triple assets, signatures, immutable publication refs, and embedded phoxal-bus/zenoh versions".to_string(),
        },
    }
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

fn release_projection(output: &PackagedOutput) -> ReleaseProjection {
    ReleaseProjection {
        asset_filename: output.tarball_name.clone(),
        asset_sha256: output.tarball_sha256.clone(),
        metadata_filename: output.metadata_name.clone(),
        metadata_sha256: output.metadata_sha256.clone(),
        checksum_filename: output.checksum_name.clone(),
    }
}

fn previous_entries_by_package(
    catalog: &CatalogRevision,
) -> Result<BTreeMap<String, &CatalogEntry>> {
    let mut entries = BTreeMap::new();
    for entry in &catalog.entries {
        if entries.insert(entry.package.clone(), entry).is_some() {
            bail!(
                "previous catalog contains duplicate package {}",
                entry.package
            );
        }
    }
    Ok(entries)
}

fn schemas_by_catalog_entries(
    entries: &[CatalogEntry],
) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    let mut schemas = BTreeMap::<String, BTreeMap<String, String>>::new();
    for entry in entries {
        let generation_schemas = schemas.entry(entry.api_generation.clone()).or_default();
        for contract in &entry.contract_uses {
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

fn schemas_by_generation_from_projections(
    projections: &BTreeMap<String, ArtifactProjection>,
) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    let mut schemas = BTreeMap::<String, BTreeMap<String, String>>::new();
    for (package, projection) in projections {
        for contract in projection.metadata.required_contracts() {
            let family = contract
                .family
                .as_ref()
                .context("validated emit-apis metadata lost family")?;
            let schema_id = contract
                .schema_id
                .as_ref()
                .context("validated emit-apis metadata lost schema_id")?;
            let generation = contract
                .api_version
                .as_ref()
                .context("validated emit-apis metadata lost api_version")?;
            let generation_schemas = schemas.entry(generation.clone()).or_default();
            match generation_schemas.get(family) {
                Some(existing) if existing != schema_id => bail!(
                    "{package} reports schema_id {} for {family}, but generation {generation} already has {}",
                    schema_id,
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

fn contract_uses(metadata: &ProjectionMetadata, package: &str) -> Result<Vec<ContractUse>> {
    let required_contracts = metadata.required_contracts();
    let mut uses = Vec::with_capacity(required_contracts.len());
    for contract in required_contracts {
        uses.push(ContractUse {
            family: contract
                .family
                .clone()
                .with_context(|| format!("{package} contract missing family"))?,
            topic_template: contract
                .topic
                .clone()
                .with_context(|| format!("{package} contract missing topic"))?,
            direction: contract
                .direction
                .clone()
                .with_context(|| format!("{package} contract missing direction"))?,
            schema_id: contract
                .schema_id
                .clone()
                .with_context(|| format!("{package} contract missing schema_id"))?,
        });
    }
    uses.sort();
    Ok(uses)
}

fn changed_contracts(
    api_generation: &str,
    contract_uses: &[ContractUse],
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
        return contract_uses
            .iter()
            .map(|contract| contract.family.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    };

    contract_uses
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

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    pub(crate) fn fixture_catalog() -> Result<CatalogRevision> {
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
        let mut projections = BTreeMap::new();
        projections.insert(
            artifact.package.clone(),
            ArtifactProjection {
                metadata: ProjectionMetadata::EmitApis(metadata),
                releases: BTreeMap::new(),
                status: BTreeMap::new(),
            },
        );
        build_revision_from_projections(
            &[artifact],
            &[ApiGeneration {
                name: "y2026_1".to_string(),
                channel: GenerationChannel::Stable,
            }],
            &projections,
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

    fn write_packaged_fixture(
        dir: &Path,
        artifact: &OfficialArtifact,
        triple: &str,
        metadata: &str,
    ) -> Result<()> {
        fs::create_dir_all(dir)?;
        let stem = package::asset_stem(artifact, triple);
        let asset = dir.join(format!("{stem}.tar.zst"));
        let checksum = dir.join(format!("{stem}.tar.zst.sha256"));
        let metadata_path = dir.join(format!("{stem}.emit-apis.json"));
        fs::write(&asset, b"fake tarball")?;
        fs::write(&metadata_path, metadata)?;
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
        package_dir: &Path,
    ) -> Result<CatalogRevision> {
        build_catalog_revision_from_artifacts(
            Path::new("."),
            artifacts,
            &generations(),
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
        write_packaged_fixture(
            temp.path(),
            &service,
            "x86_64-unknown-linux-gnu",
            &emit_json("drive", "service", "drive::Target", "0123456789abcdef"),
        )?;

        let catalog = build_from_outputs(&[service], temp.path())?;
        catalog.verify()?;
        let json = serde_json::to_string_pretty(&catalog)?;
        let reparsed: CatalogRevision = serde_json::from_str(&json)?;
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
        write_packaged_fixture(
            temp.path(),
            &service,
            "x86_64-unknown-linux-gnu",
            &emit_json("drive", "service", "drive::Target", "0123456789abcdef"),
        )?;

        let catalog = build_from_outputs(&[service], temp.path())?;
        let entry = &catalog.entries[0];
        assert_eq!(entry.package, "phoxal/service-drive");
        assert_eq!(
            entry.status["x86_64-unknown-linux-gnu"],
            ArtifactStatus::Released
        );
        assert!(
            entry
                .release_assets
                .contains_key("x86_64-unknown-linux-gnu")
        );
        assert_eq!(entry.channels[&Channel::Stable], "0.1.0");
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
        write_packaged_fixture(
            temp.path(),
            &service,
            "x86_64-unknown-linux-gnu",
            &emit_json("drive", "service", "drive::Target", "0123456789abcdef"),
        )?;

        let err = build_from_outputs(&[service, driver], temp.path()).unwrap_err();
        assert_error_contains(&err, "missing packaged output");
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
        write_packaged_fixture(
            temp.path(),
            &service,
            "x86_64-unknown-linux-gnu",
            &emit_json("drive", "service", "drive::Target", "0123456789abcdef"),
        )?;

        let expected = build_from_outputs(std::slice::from_ref(&service), temp.path())?;
        let mut edited = expected.clone();
        edited.entries[0].contract_uses[0].schema_id = "1111111111111111".to_string();
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
        write_packaged_fixture(
            temp.path(),
            &service,
            "x86_64-unknown-linux-gnu",
            &emit_json("drive", "service", "drive::Target", "0123456789abcdef"),
        )?;

        let expected = build_from_outputs(std::slice::from_ref(&service), temp.path())?;
        let mut edited = expected.clone();
        edited.entries[0]
            .launch_facts
            .systemd
            .groups
            .push("dialout".to_string());
        edited = edited.finalize()?;
        let err = compare_catalogs(&edited, &expected).unwrap_err();
        assert_error_contains(&err, "hand-edit/provenance drift");
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
