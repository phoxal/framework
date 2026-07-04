use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use sha2::{Digest, Sha256};

use crate::api::sync_features::{ApiGeneration, GenerationChannel, api_generations_from_workspace};
use crate::catalog::schema::{
    ArtifactStatus, CatalogEntry, CatalogProvenance, CatalogRevision, Channel, ContractUse,
    EngineVersions, LaunchFacts, ReleaseAsset, ReleaseAssetMetadata, RouterFacts, SystemdFacts,
};
use crate::release::package::{self, EmitApisMetadata};
use crate::workspace::{ArtifactKind, OfficialArtifact, Workspace, require_nonempty_artifacts};

const DEFAULT_CATALOG_OUT: &str = "target/xtask/catalog/phoxal-artifact-catalog.json";
const DEFAULT_PACKAGE_DIR: &str = "target/xtask/release";

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long, value_name = "PATH", default_value = DEFAULT_CATALOG_OUT)]
    pub out: PathBuf,
    #[arg(long, value_name = "DIR", default_value = DEFAULT_PACKAGE_DIR)]
    pub package_dir: PathBuf,
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
}

#[derive(Debug)]
struct ArtifactProjection {
    metadata: EmitApisMetadata,
    release: Option<ReleaseProjection>,
    status: ArtifactStatus,
}

#[derive(Debug)]
struct ReleaseProjection {
    asset_filename: String,
    asset_sha256: String,
    metadata_filename: String,
    metadata_sha256: String,
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

    if mode == InputMode::PackageOutputs {
        fs::create_dir_all(&package_dir)
            .with_context(|| format!("failed to create {}", package_dir.display()))?;
        package::package_artifacts(&workspace, artifacts, &package_dir, &host_triple)?;
    }

    let revision = build_catalog_revision(
        &workspace,
        &GenerateOptions { package_dir, mode },
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
    let projections = projections(root, artifacts, options, host_triple)?;
    build_revision_from_projections(
        artifacts,
        api_generations,
        &projections,
        options.mode,
        host_triple,
    )
}

fn projections(
    root: &Path,
    artifacts: &[OfficialArtifact],
    options: &GenerateOptions,
    host_triple: &str,
) -> Result<BTreeMap<String, ArtifactProjection>> {
    let mut projections = BTreeMap::new();
    for artifact in artifacts {
        let projection = match options.mode {
            InputMode::PackageOutputs => {
                projection_from_package_outputs(artifact, &options.package_dir, host_triple)?
            }
            InputMode::MetadataOnly => projection_from_emit_apis(root, artifact)?,
        };
        projections.insert(artifact.package_name.clone(), projection);
    }
    Ok(projections)
}

fn projection_from_package_outputs(
    artifact: &OfficialArtifact,
    package_dir: &Path,
    host_triple: &str,
) -> Result<ArtifactProjection> {
    let stem = package::asset_stem(artifact, host_triple);
    let asset = package_dir.join(format!("{stem}.tar.zst"));
    let checksum = package_dir.join(format!("{stem}.tar.zst.sha256"));
    let metadata = package_dir.join(format!("{stem}.emit-apis.json"));
    for path in [&asset, &checksum, &metadata] {
        if !path.is_file() {
            bail!(
                "missing packaged output for {}: {}",
                artifact.package_name,
                path.display()
            );
        }
    }

    let recorded = read_checksum_file(&checksum, &asset)?;
    let computed = package::sha256_file(&asset)?;
    if recorded != computed {
        bail!(
            "{} checksum file recorded {}, but computed {}",
            asset.display(),
            recorded,
            computed
        );
    }

    let metadata_bytes =
        fs::read(&metadata).with_context(|| format!("failed to read {}", metadata.display()))?;
    let parsed = package::parse_emit_apis_json(&metadata_bytes, &artifact.id, artifact.kind)
        .with_context(|| format!("invalid emit-apis metadata {}", metadata.display()))?;

    Ok(ArtifactProjection {
        metadata: parsed,
        release: Some(ReleaseProjection {
            asset_filename: file_name(&asset)?,
            asset_sha256: computed,
            metadata_filename: file_name(&metadata)?,
            metadata_sha256: sha256_bytes(&metadata_bytes),
            checksum_filename: file_name(&checksum)?,
        }),
        status: ArtifactStatus::Released,
    })
}

fn projection_from_emit_apis(
    root: &Path,
    artifact: &OfficialArtifact,
) -> Result<ArtifactProjection> {
    let stdout = package::emit_apis_from_cargo_run(root, artifact)?;
    let parsed = package::parse_emit_apis_json(&stdout, &artifact.id, artifact.kind)
        .with_context(|| format!("invalid emit-apis metadata from {}", artifact.package_name))?;
    Ok(ArtifactProjection {
        metadata: parsed,
        release: None,
        status: ArtifactStatus::Pending,
    })
}

fn build_revision_from_projections(
    artifacts: &[OfficialArtifact],
    api_generations: &[ApiGeneration],
    projections: &BTreeMap<String, ArtifactProjection>,
    mode: InputMode,
    host_triple: &str,
) -> Result<CatalogRevision> {
    let channels_by_generation = channels_by_generation(api_generations)?;
    let schemas_by_generation = schemas_by_generation(projections)?;
    let mut entries = Vec::with_capacity(artifacts.len());

    for artifact in artifacts {
        let projection = projections
            .get(&artifact.package_name)
            .with_context(|| format!("missing projection for {}", artifact.package_name))?;
        let metadata = &projection.metadata;
        let api_generation = metadata.api_version.clone();
        let channel = channels_by_generation
            .get(&api_generation)
            .with_context(|| {
                format!(
                    "{} emitted unknown api_generation '{}'",
                    artifact.package_name, api_generation
                )
            })?;
        let contract_uses = contract_uses(metadata, &artifact.package_name)?;
        let changed_contracts = changed_contracts(
            &api_generation,
            &contract_uses,
            api_generations,
            &schemas_by_generation,
        );

        let mut release_assets = BTreeMap::new();
        if let Some(release) = &projection.release {
            release_assets.insert(
                host_triple.to_string(),
                ReleaseAsset {
                    asset: release.asset_filename.clone(),
                    sha256: release.asset_sha256.clone(),
                    metadata: ReleaseAssetMetadata {
                        emit_apis: release.metadata_filename.clone(),
                        emit_apis_sha256: release.metadata_sha256.clone(),
                        sha256_file: release.checksum_filename.clone(),
                    },
                },
            );
        }

        let mut status = BTreeMap::new();
        status.insert(host_triple.to_string(), projection.status);

        let mut channels = BTreeMap::new();
        channels.insert(*channel, artifact.version.clone());

        entries.push(CatalogEntry {
            artifact_id: catalog_artifact_id(artifact.kind, &artifact.id),
            kind: artifact.kind,
            package: artifact.package_name.clone(),
            version: artifact.version.clone(),
            api_generation,
            contract_uses,
            target_triples: vec![host_triple.to_string()],
            release_assets,
            launch_facts: LaunchFacts {
                participant_kind: metadata.artifact.kind.clone(),
                participant_class: metadata
                    .participant_class
                    .clone()
                    .context("validated emit-apis metadata lost participant_class")?,
                router: RouterFacts {
                    needs_zenoh_router: true,
                },
                systemd: SystemdFacts {
                    groups: Vec::new(),
                    devices: Vec::new(),
                    source: "not-modeled-until-native-release-plan-01".to_string(),
                },
            },
            engine_versions: EngineVersions {
                phoxal: Some(metadata.framework.version.clone()),
                phoxal_bus: None,
                zenoh: None,
            },
            channels,
            status,
            changed_contracts,
        });
    }

    entries.sort_by(|left, right| left.artifact_id.cmp(&right.artifact_id));

    CatalogRevision::new(provenance(mode), entries).finalize()
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

fn schemas_by_generation(
    projections: &BTreeMap<String, ArtifactProjection>,
) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    let mut schemas = BTreeMap::<String, BTreeMap<String, String>>::new();
    for (package_name, projection) in projections {
        for contract in &projection.metadata.required_contracts {
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
                    "{package_name} reports schema_id {} for {family}, but generation {generation} already has {}",
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

fn contract_uses(metadata: &EmitApisMetadata, package_name: &str) -> Result<Vec<ContractUse>> {
    let mut uses = Vec::with_capacity(metadata.required_contracts.len());
    for contract in &metadata.required_contracts {
        uses.push(ContractUse {
            family: contract
                .family
                .clone()
                .with_context(|| format!("{package_name} contract missing family"))?,
            topic_template: contract
                .topic
                .clone()
                .with_context(|| format!("{package_name} contract missing topic"))?,
            direction: contract
                .direction
                .clone()
                .with_context(|| format!("{package_name} contract missing direction"))?,
            schema_id: contract
                .schema_id
                .clone()
                .with_context(|| format!("{package_name} contract missing schema_id"))?,
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

fn read_checksum_file(checksum: &Path, asset: &Path) -> Result<String> {
    let text = fs::read_to_string(checksum)
        .with_context(|| format!("failed to read {}", checksum.display()))?;
    let mut parts = text.split_whitespace();
    let digest = parts
        .next()
        .with_context(|| format!("{} is empty", checksum.display()))?;
    let filename = parts
        .next()
        .with_context(|| format!("{} is missing an asset filename", checksum.display()))?;
    let expected = asset
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("{} has no UTF-8 filename", asset.display()))?;
    if filename != expected {
        bail!(
            "{} names asset '{}', expected '{}'",
            checksum.display(),
            filename,
            expected
        );
    }
    Ok(digest.to_string())
}

fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .with_context(|| format!("{} has no UTF-8 filename", path.display()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(crate) fn catalog_artifact_id(kind: ArtifactKind, id: &str) -> String {
    match kind {
        ArtifactKind::Service => format!("service-{id}"),
        ArtifactKind::Driver => format!("driver-{id}"),
        ArtifactKind::Tool => format!("tool-{id}"),
        ArtifactKind::Simulator => format!("simulator-{id}"),
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    pub(crate) fn fixture_catalog() -> Result<CatalogRevision> {
        let artifact = OfficialArtifact {
            package_name: "phoxal-service-drive".to_string(),
            kind: ArtifactKind::Service,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::new(),
            bin_name: "phoxal-service-drive".to_string(),
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
            artifact.package_name.clone(),
            ArtifactProjection {
                metadata,
                release: None,
                status: ArtifactStatus::Pending,
            },
        );
        build_revision_from_projections(
            &[artifact],
            &[ApiGeneration {
                name: "y2026_1".to_string(),
                channel: GenerationChannel::Stable,
            }],
            &projections,
            InputMode::MetadataOnly,
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
            package_name: package_name.to_string(),
            kind,
            version: version.to_string(),
            crate_dir: PathBuf::new(),
            bin_name: package_name.to_string(),
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
        assert_eq!(entry.artifact_id, "service-drive");
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
            "phoxal-driver-ddsm115",
            "ddsm115",
            ArtifactKind::Driver,
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
