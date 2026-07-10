//! `cargo xtask catalog generate` - assemble-from-facts, never build.
//!
//! Design doc `organization/tmp/ci-release-refactor/design.md` §4.2: this
//! command never invokes `cargo build`/`cargo auditable build` and never
//! executes a binary (except the `--metadata-only` PR-gate mode, which does a
//! plain host build purely to materialize the `#[derive(phoxal::Api)]` linker
//! section - see [`crate::release::package::build_and_extract_metadata`]).
//! Everything else is read straight off facts already on disk: the packaged
//! tarballs + checksums in `--package-dir` (produced by `release package`)
//! and the contract metadata embedded in those same tarballs.
//!
//! The merge is keyed by `(package, version)` (§4.2): start from
//! `--previous-catalog`'s full `artifacts[]` (empty if absent - cold start,
//! §4.3), then upsert one fresh entry per `(package, version)` this run has
//! facts for in `--package-dir`. Nothing is dropped, refreshed, or reshaped;
//! `(package, version)` is unique, so re-running generate against the same
//! `--package-dir` is idempotent.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use semver::Version;

use phoxal::check::{ParticipantContractSurface, check_coherence};
use phoxal::participant::metadata::ParticipantMetaContract;

use super::model::{Artifact as CatalogArtifact, Blob, BuildProvenance, Catalog, Contract, Heads};
use crate::release::metadata::ParticipantMeta;
use crate::release::package::{self, PackagedOutput};
use crate::release::plan::{ReleasePlan, load_release_plan};
use crate::workspace::{
    OfficialArtifact, TARGET_INDEPENDENT_SCOPE, Workspace, require_nonempty_artifacts,
};

const DEFAULT_CATALOG_OUT: &str = "target/xtask/catalog/catalog.json";
const DEFAULT_PACKAGE_DIR: &str = "target/xtask/release";

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long, value_name = "PATH", default_value = DEFAULT_CATALOG_OUT)]
    pub out: PathBuf,
    #[arg(long, value_name = "DIR", default_value = DEFAULT_PACKAGE_DIR)]
    pub package_dir: PathBuf,
    #[arg(long, value_name = "PATH")]
    pub release_plan: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub previous_catalog: Option<PathBuf>,
    /// Generate from a plain host build of each artifact (extracting its
    /// compiled-in `#[derive(phoxal::Api)]` metadata section), without
    /// release builds or tarballs. CI uses this as the cheap PR gate - the
    /// resulting entries carry `contracts[]` but no `targets`/`assets`
    /// (there is nothing built to point a `Blob` at).
    #[arg(long)]
    pub metadata_only: bool,
    /// `owner/repo` this run's fresh `Blob.url`s are formed against.
    #[arg(
        long,
        value_name = "OWNER/REPO",
        env = "GITHUB_REPOSITORY",
        default_value = "phoxal/framework"
    )]
    pub repo: String,
    /// The `build-*` release tag this run's fresh tarballs were uploaded to.
    #[arg(long, value_name = "TAG")]
    pub build_tag: String,
    #[arg(long)]
    pub run_number: u64,
    #[arg(long)]
    pub run_id: u64,
    #[arg(long, value_name = "SHA")]
    pub commit: String,
    #[arg(long, value_name = "RFC3339")]
    pub created_at: String,
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
    pub repo: String,
    pub build_tag: String,
    pub release_plan: Option<ReleasePlan>,
    pub previous_catalog: Option<Catalog>,
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

    let build = BuildProvenance {
        tag: args.build_tag.clone(),
        run_number: args.run_number,
        run_id: args.run_id,
        commit: args.commit.clone(),
        created_at: args.created_at.clone(),
    };
    let options = GenerateOptions {
        package_dir,
        mode,
        repo: args.repo.clone(),
        build_tag: args.build_tag.clone(),
        release_plan,
        previous_catalog,
    };

    let catalog = build_catalog(&workspace, artifacts, &options, build)?;
    write_catalog(&out, &catalog)?;
    println!(
        "generated catalog {} with {} artifact entries at {}",
        catalog.schema,
        catalog.artifacts.len(),
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

pub(crate) fn write_catalog(path: &Path, catalog: &Catalog) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut json = serde_json::to_string_pretty(catalog).context("failed to serialize catalog")?;
    json.push('\n');
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}

/// Builds the full-index catalog: `options.previous_catalog`'s entries merged
/// with fresh entries for every artifact this run has facts for, plus the
/// coherence-gate design doc §4 `heads` computed over that merged set.
pub(crate) fn build_catalog(
    workspace: &Workspace,
    artifacts: &[OfficialArtifact],
    options: &GenerateOptions,
    build: BuildProvenance,
) -> Result<Catalog> {
    if let Some(plan) = &options.release_plan {
        validate_release_plan_facts(plan, artifacts, options)?;
    }
    let merged = merge_artifacts(workspace, artifacts, options)?;
    let heads = compute_heads(&merged, options);
    Ok(Catalog::new(build, merged, heads))
}

// ---------------------------------------------------------------------------
// Heads (coherence-gate design doc §4: whole-set snapshot pointers)
// ---------------------------------------------------------------------------

/// Computes the coherence-gate design doc §4 `heads`: `nightly` is always
/// this run's build tag; `stable` is this run's build tag iff the
/// **latest-version set** of `merged` (the would-be-deployed default set -
/// [`latest_version_surfaces`]) passes `phoxal::check::check_coherence`,
/// else it is carried forward from `options.previous_catalog`'s `stable`
/// (empty if there is no previous catalog or its `stable` was itself empty -
/// cold start with an incoherent set has no coherent snapshot to point at,
/// design doc §4.3/§7 decision 3).
///
/// `--metadata-only` mode (the PR gate, not a publish - see the module docs)
/// has no build snapshot to point at, so both heads stay empty regardless of
/// coherence.
fn compute_heads(merged: &[CatalogArtifact], options: &GenerateOptions) -> Heads {
    if options.mode == InputMode::MetadataOnly {
        return Heads::empty();
    }

    let surfaces = latest_version_surfaces(merged);
    let report = check_coherence(&surfaces);
    let nightly = options.build_tag.clone();

    let stable = if report.is_ok() {
        options.build_tag.clone()
    } else {
        let previous_stable = options
            .previous_catalog
            .as_ref()
            .map(|catalog| catalog.heads.stable.clone())
            .unwrap_or_default();
        if previous_stable.is_empty() {
            eprintln!(
                "warning: the latest-version set is incoherent and no previous coherent \
                 snapshot exists - heads.stable stays empty until a coherent snapshot is \
                 published (coherence-gate design doc §4)"
            );
        }
        previous_stable
    };

    Heads { stable, nightly }
}

/// Reduces `artifacts` to one [`ParticipantContractSurface`] per package's
/// **latest version** (by semver) - the would-be-deployed default set the
/// design doc §4 heads are computed over, not the whole historical index.
/// Entries with an empty `contracts[]` (asset-only artifacts, which carry no
/// `#[derive(phoxal::Api)]` section - model doc on [`CatalogArtifact::contracts`])
/// are skipped: they cannot affect coherence, so including them would only be
/// a no-op.
fn latest_version_surfaces(artifacts: &[CatalogArtifact]) -> Vec<ParticipantContractSurface> {
    let mut latest: BTreeMap<&str, &CatalogArtifact> = BTreeMap::new();
    for artifact in artifacts {
        latest
            .entry(artifact.package.as_str())
            .and_modify(|current| {
                if version_newer(&artifact.version, &current.version) {
                    *current = artifact;
                }
            })
            .or_insert(artifact);
    }

    latest
        .into_values()
        .filter(|artifact| !artifact.contracts.is_empty())
        .map(|artifact| ParticipantContractSurface {
            participant_id: artifact.package.clone(),
            contracts: artifact
                .contracts
                .iter()
                .map(|contract| ParticipantMetaContract {
                    role: contract.role.clone(),
                    generation: contract.generation.clone(),
                    contract: contract.contract.clone(),
                    external: contract.external,
                })
                .collect(),
        })
        .collect()
}

/// Whether `candidate` is a newer version than `current`. Parses both as
/// semver (every official artifact's version comes from its `Cargo.toml`, so
/// this should always succeed); falls back to a plain string comparison if
/// either fails to parse, rather than panicking on a malformed fixture/hand
/// edited catalog.
fn version_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Ok(candidate), Ok(current)) => candidate > current,
        _ => candidate > current,
    }
}

/// A release plan names the packages this run was *supposed* to build; if one
/// of them has no packaged facts on disk, the run is broken and generate
/// should fail loudly here rather than silently omitting the entry (which
/// `catalog check`'s coverage gate would only catch later, with a less
/// specific error).
fn validate_release_plan_facts(
    plan: &ReleasePlan,
    artifacts: &[OfficialArtifact],
    options: &GenerateOptions,
) -> Result<()> {
    if options.mode == InputMode::MetadataOnly {
        return Ok(());
    }
    for planned in &plan.artifacts {
        let artifact = artifacts
            .iter()
            .find(|artifact| artifact.package == planned.package)
            .with_context(|| {
                format!(
                    "release plan references unknown package {}",
                    planned.package
                )
            })?;
        if !artifact_has_new_facts(artifact, options)? {
            bail!(
                "release plan expected {} v{} to be packaged this run, but no packaged output \
                 was found in {}",
                artifact.package,
                artifact.version,
                options.package_dir.display()
            );
        }
    }
    Ok(())
}

fn merge_artifacts(
    workspace: &Workspace,
    artifacts: &[OfficialArtifact],
    options: &GenerateOptions,
) -> Result<Vec<CatalogArtifact>> {
    let mut merged: BTreeMap<(String, String), CatalogArtifact> = BTreeMap::new();
    if let Some(previous) = &options.previous_catalog {
        for entry in &previous.artifacts {
            merged.insert(
                (entry.package.clone(), entry.version.clone()),
                entry.clone(),
            );
        }
    }

    for artifact in artifacts {
        if !artifact_has_new_facts(artifact, options)? {
            continue;
        }
        let entry = build_entry(workspace, artifact, options)?;
        merged.insert((artifact.package.clone(), artifact.version.clone()), entry);
    }

    Ok(merged.into_values().collect())
}

/// Whether `artifact`'s *current* version has facts on disk this run: in
/// `MetadataOnly` mode every artifact always does (a fresh host build always
/// happens); in `PackageOutputs` mode, whether at least one packaged tarball
/// (binary target or, for a [`ArtifactKind::Component`], the target-independent
/// asset bundle) is present in `--package-dir`.
fn artifact_has_new_facts(artifact: &OfficialArtifact, options: &GenerateOptions) -> Result<bool> {
    match options.mode {
        InputMode::MetadataOnly => Ok(true),
        InputMode::PackageOutputs => Ok(artifact
            .supported_target_triples()
            .iter()
            .any(|triple| packaged_tarball_path(artifact, &options.package_dir, triple).is_file())),
    }
}

fn packaged_tarball_path(artifact: &OfficialArtifact, package_dir: &Path, triple: &str) -> PathBuf {
    let stem = package::asset_stem(artifact, triple);
    package_dir.join(format!("{stem}.tar.zst"))
}

/// `artifact`'s binary target triples this run - every [`OfficialArtifact::supported_target_triples`]
/// entry except [`TARGET_INDEPENDENT_SCOPE`] (a [`ArtifactKind::Component`]'s
/// asset-bundle output is never a binary and has no `#[derive(phoxal::Api)]`
/// section to extract).
fn binary_target_triples(artifact: &OfficialArtifact) -> Vec<String> {
    artifact
        .supported_target_triples()
        .into_iter()
        .filter(|triple| triple != TARGET_INDEPENDENT_SCOPE)
        .collect()
}

fn build_entry(
    workspace: &Workspace,
    artifact: &OfficialArtifact,
    options: &GenerateOptions,
) -> Result<CatalogArtifact> {
    let binary_triples = binary_target_triples(artifact);
    // `contracts[]` is present iff this run actually produced a binary output
    // for `artifact` (model doc: "present iff the crate has a binary"). Every
    // discovered artifact is crate-backed, but a given run may have packaged
    // only the target-independent asset bundle for a `Component` (e.g. a
    // partial run) - in that case there is no binary to extract metadata
    // from, so contracts stay empty rather than erroring.
    let has_binary_output = match options.mode {
        InputMode::MetadataOnly => true,
        InputMode::PackageOutputs => binary_triples
            .iter()
            .any(|triple| packaged_tarball_path(artifact, &options.package_dir, triple).is_file()),
    };
    let contracts = if has_binary_output {
        contracts_from_metadata(&extract_metadata(
            workspace,
            artifact,
            options,
            &binary_triples,
        )?)
    } else {
        Vec::new()
    };

    let mut targets = BTreeMap::new();
    let mut assets = None;

    if options.mode == InputMode::PackageOutputs {
        for triple in artifact.supported_target_triples() {
            if !packaged_tarball_path(artifact, &options.package_dir, &triple).is_file() {
                continue;
            }
            let output = package::read_packaged_output(artifact, &options.package_dir, &triple)?;
            let blob = blob_from_output(options, &output);
            if triple == TARGET_INDEPENDENT_SCOPE {
                assets = Some(blob);
            } else {
                targets.insert(triple, blob);
            }
        }
    }

    Ok(CatalogArtifact {
        package: artifact.package.clone(),
        version: artifact.version.clone(),
        contracts,
        // Placeholder until the config JSON Schema gets a host-side `build.rs`
        // materialization step (design doc §8/§10) - omit rather than fake it.
        config_schema: None,
        targets,
        assets,
    })
}

fn extract_metadata(
    workspace: &Workspace,
    artifact: &OfficialArtifact,
    options: &GenerateOptions,
    binary_triples: &[String],
) -> Result<ParticipantMeta> {
    match options.mode {
        InputMode::PackageOutputs => {
            package::extract_metadata_from_packaged(artifact, &options.package_dir, binary_triples)
        }
        InputMode::MetadataOnly => package::build_and_extract_metadata(workspace, artifact)
            .with_context(|| format!("failed to extract API metadata for {}", artifact.package)),
    }
}

fn blob_from_output(options: &GenerateOptions, output: &PackagedOutput) -> Blob {
    Blob {
        url: format!(
            "https://github.com/{}/releases/download/{}/{}",
            options.repo, options.build_tag, output.tarball_name
        ),
        sha256: output.tarball_sha256.clone(),
        size: output.tarball_size,
    }
}

/// Projects an extracted `#[derive(phoxal::Api)]` manifest into the catalog's
/// `Contract` list: generation + contract + role + external, deduplicated (a
/// `Server<Req, Resp>` field contributes two entries that may collide with
/// another field's entries in the same role).
fn contracts_from_metadata(meta: &ParticipantMeta) -> Vec<Contract> {
    let mut contracts: Vec<Contract> = meta
        .contracts
        .iter()
        .map(|entry| Contract {
            generation: entry.generation.clone(),
            contract: entry.contract.clone(),
            role: entry.role.clone(),
            external: entry.external,
        })
        .collect();
    contracts.sort();
    contracts.dedup();
    contracts
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;

    use sha2::{Digest, Sha256};

    use super::*;
    use crate::release::metadata::ParticipantMetaContract;
    use crate::workspace::ArtifactKind;

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

    /// A `Component` test fixture. Tests below only ever write its
    /// [`TARGET_INDEPENDENT_SCOPE`] tarball to disk (never a binary-target
    /// one), so `build_entry`'s "no binary output this run" branch keeps
    /// `contracts` empty and no metadata extraction (which would need a real
    /// object file) is attempted - see `build_entry`'s `has_binary_output`.
    fn component_artifact(id: &str, version: &str) -> OfficialArtifact {
        OfficialArtifact {
            package: crate::workspace::package_identity(ArtifactKind::Component, id),
            package_name: Some(format!("phoxal-component-{id}")),
            kind: ArtifactKind::Component,
            version: version.to_string(),
            crate_dir: PathBuf::new(),
            bin_name: Some(format!("phoxal-component-{id}")),
            id: id.to_string(),
            metadata: Default::default(),
        }
    }

    /// Writes a fake packaged tarball + checksum straight into `dir`, exactly
    /// as `release package` would have left them.
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

    fn fixture_workspace() -> Workspace {
        Workspace::from_parts_for_tests(
            PathBuf::from("/repo"),
            PathBuf::from("/repo/target"),
            Vec::new(),
        )
    }

    fn base_options(package_dir: &Path) -> GenerateOptions {
        GenerateOptions {
            package_dir: package_dir.to_path_buf(),
            mode: InputMode::PackageOutputs,
            repo: "phoxal/framework".to_string(),
            build_tag: "build-20260708-0001234".to_string(),
            release_plan: None,
            previous_catalog: None,
        }
    }

    /// Cold start (design §4.3): no previous catalog, this run's package_dir
    /// carries the only facts, so the catalog is exactly this run's builds.
    #[test]
    fn cold_start_assembles_from_facts_alone() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let service = artifact(
            "phoxal-service-drive",
            "drive",
            ArtifactKind::Service,
            "0.1.0",
        );
        for triple in service.supported_target_triples() {
            write_packaged_fixture(temp.path(), &service, &triple)?;
        }

        // extract_metadata_from_packaged reads the tarball bytes as an
        // object file; a "fake tarball" won't parse as one, so drive this
        // test through the fixture's `.tar.zst` bytes being a *real* (if
        // trivial) archive is unnecessary - metadata extraction is exercised
        // end-to-end in `release::package`'s own tests. Here we only need
        // `merge_artifacts`' bookkeeping, so only package the component's
        // target-independent asset tarball (no binary target) to keep the
        // fixture simple - see `component_artifact`'s doc comment.
        let assets = component_artifact("ddsm115", "0.1.0");
        write_packaged_fixture(temp.path(), &assets, TARGET_INDEPENDENT_SCOPE)?;

        let workspace = fixture_workspace();
        let options = base_options(temp.path());
        let merged = merge_artifacts(&workspace, std::slice::from_ref(&assets), &options)?;

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].package, "phoxal/component-ddsm115");
        assert_eq!(merged[0].version, "0.1.0");
        assert!(merged[0].targets.is_empty());
        assert!(merged[0].contracts.is_empty());
        let blob = merged[0].assets.as_ref().expect("assets blob present");
        assert_eq!(
            blob.url,
            "https://github.com/phoxal/framework/releases/download/build-20260708-0001234/\
             phoxal-component-ddsm115-v0.1.0-target-independent.tar.zst"
        );
        assert_eq!(blob.size, b"fake tarball".len() as u64);
        Ok(())
    }

    /// Merge/append (design §4.2): previous catalog has `(pkg, 0.1.0)`; this
    /// run adds `(pkg, 0.2.0)` for a *different* package (the workspace only
    /// discovers the crate's current version, never an older one) - both end
    /// up in the merged index.
    #[test]
    fn merge_appends_new_versions_without_dropping_old_ones() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let assets_v2 = component_artifact("ddsm115", "0.2.0");
        write_packaged_fixture(temp.path(), &assets_v2, TARGET_INDEPENDENT_SCOPE)?;

        let previous = Catalog::new(
            fixture_build(),
            vec![CatalogArtifact {
                package: "phoxal/component-ddsm115".to_string(),
                version: "0.1.0".to_string(),
                contracts: Vec::new(),
                config_schema: None,
                targets: BTreeMap::new(),
                assets: Some(Blob {
                    url:
                        "https://github.com/phoxal/framework/releases/download/build-old/a.tar.zst"
                            .to_string(),
                    sha256: "b".repeat(64),
                    size: 7,
                }),
            }],
            Heads::empty(),
        );

        let workspace = fixture_workspace();
        let mut options = base_options(temp.path());
        options.previous_catalog = Some(previous);
        let merged = merge_artifacts(&workspace, std::slice::from_ref(&assets_v2), &options)?;

        assert_eq!(merged.len(), 2);
        let versions: Vec<&str> = merged.iter().map(|entry| entry.version.as_str()).collect();
        assert_eq!(versions, vec!["0.1.0", "0.2.0"]);
        // The carried-forward 0.1.0 entry keeps its original (older) URL.
        assert_eq!(
            merged[0].assets.as_ref().unwrap().url,
            "https://github.com/phoxal/framework/releases/download/build-old/a.tar.zst"
        );
        Ok(())
    }

    /// Re-running generate against the same `package_dir`/`previous_catalog`
    /// produces byte-identical output: `(package, version)` is unique, so
    /// the merge is idempotent.
    #[test]
    fn regenerating_from_the_same_facts_is_idempotent() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let assets = component_artifact("ddsm115", "0.1.0");
        write_packaged_fixture(temp.path(), &assets, TARGET_INDEPENDENT_SCOPE)?;

        let workspace = fixture_workspace();
        let options = base_options(temp.path());
        let first = merge_artifacts(&workspace, std::slice::from_ref(&assets), &options)?;
        let second = merge_artifacts(&workspace, std::slice::from_ref(&assets), &options)?;
        assert_eq!(first, second);
        Ok(())
    }

    /// A release plan naming a package with no packaged output in
    /// `package_dir` is a broken run and generate must fail fast.
    #[test]
    fn release_plan_facts_are_required_when_present() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let assets = component_artifact("ddsm115", "0.1.0");
        // No fixture written: the plan claims it was built, but it wasn't.

        let plan = ReleasePlan {
            schema: crate::release::plan::RELEASE_PLAN_SCHEMA.to_string(),
            artifacts: vec![crate::release::plan::ReleasePlanArtifact {
                package: assets.package.clone(),
                version: assets.version.clone(),
                tag: assets.release_tag(),
                kind: ArtifactKind::Component,
                target_triples: vec![TARGET_INDEPENDENT_SCOPE.to_string()],
            }],
            matrix: crate::release::plan::ReleaseMatrix {
                include: Vec::new(),
            },
        };

        let workspace = fixture_workspace();
        let options = base_options(temp.path());
        let err = build_catalog(
            &workspace,
            std::slice::from_ref(&assets),
            &GenerateOptions {
                release_plan: Some(plan),
                ..options
            },
            fixture_build(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("no packaged output was found"));
        Ok(())
    }

    #[test]
    fn contracts_from_metadata_deduplicates_and_sorts() {
        let meta = ParticipantMeta {
            participant_api: "Api".to_string(),
            contracts: vec![
                ParticipantMetaContract {
                    role: "publish".to_string(),
                    generation: "y2026_1".to_string(),
                    contract: "drive::Target".to_string(),
                    external: false,
                },
                ParticipantMetaContract {
                    role: "publish".to_string(),
                    generation: "y2026_1".to_string(),
                    contract: "drive::Target".to_string(),
                    external: false,
                },
            ],
        };
        let contracts = contracts_from_metadata(&meta);
        assert_eq!(
            contracts,
            vec![Contract {
                generation: "y2026_1".to_string(),
                contract: "drive::Target".to_string(),
                role: "publish".to_string(),
                external: false,
            }]
        );
    }

    /// Two entries that share `(generation, contract, role)` but disagree on
    /// `external` are NOT deduplicated - the derive itself never emits that
    /// shape (a compile error catches the disagreement earlier,
    /// `phoxal-macros/src/authoring.rs`), but this projection does not assume
    /// it and sorts/dedups on the whole `Contract` value.
    #[test]
    fn contracts_from_metadata_carries_the_external_flag() {
        let meta = ParticipantMeta {
            participant_api: "Api".to_string(),
            contracts: vec![ParticipantMetaContract {
                role: "subscribe".to_string(),
                generation: "y2026_1".to_string(),
                contract: "drive::Target".to_string(),
                external: true,
            }],
        };
        let contracts = contracts_from_metadata(&meta);
        assert_eq!(
            contracts,
            vec![Contract {
                generation: "y2026_1".to_string(),
                contract: "drive::Target".to_string(),
                role: "subscribe".to_string(),
                external: true,
            }]
        );
    }

    fn fixture_build() -> BuildProvenance {
        BuildProvenance {
            tag: "build-old".to_string(),
            run_number: 1,
            run_id: 1,
            commit: "old".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn contract_entry(role: &str, generation: &str, contract: &str, external: bool) -> Contract {
        Contract {
            generation: generation.to_string(),
            contract: contract.to_string(),
            role: role.to_string(),
            external,
        }
    }

    fn artifact_with_contracts(
        package: &str,
        version: &str,
        contracts: Vec<Contract>,
    ) -> CatalogArtifact {
        CatalogArtifact {
            package: package.to_string(),
            version: version.to_string(),
            contracts,
            config_schema: None,
            targets: BTreeMap::new(),
            assets: None,
        }
    }

    /// A coherent latest-version set: `heads.stable` and `heads.nightly` both
    /// point at this run's own build tag (design doc §4).
    #[test]
    fn compute_heads_coherent_set_points_stable_and_nightly_at_this_run() {
        let merged = vec![
            artifact_with_contracts(
                "phoxal/service-drive",
                "0.1.0",
                vec![contract_entry("publish", "y2026_1", "drive::Target", false)],
            ),
            artifact_with_contracts(
                "phoxal/service-mission",
                "0.1.0",
                vec![contract_entry(
                    "subscribe",
                    "y2026_1",
                    "drive::Target",
                    false,
                )],
            ),
        ];
        let options = base_options(Path::new("/unused"));

        let heads = compute_heads(&merged, &options);

        assert_eq!(heads.nightly, options.build_tag);
        assert_eq!(heads.stable, options.build_tag);
    }

    /// An incoherent latest-version set with a previously coherent catalog:
    /// `nightly` still advances to this run's tag, but `stable` is carried
    /// forward from the previous catalog's `stable` (design doc §4: "an
    /// incoherent snapshot is therefore nightly-only").
    #[test]
    fn compute_heads_incoherent_set_carries_forward_previous_stable() {
        let merged = vec![
            artifact_with_contracts(
                "phoxal/service-drive",
                "0.1.0",
                vec![contract_entry("publish", "y2026_1", "drive::Target", false)],
            ),
            artifact_with_contracts(
                "phoxal/service-mission",
                "0.1.0",
                vec![contract_entry(
                    "subscribe",
                    "y2026_7",
                    "drive::Target",
                    false,
                )],
            ),
        ];
        let mut options = base_options(Path::new("/unused"));
        options.previous_catalog = Some(Catalog::new(
            fixture_build(),
            Vec::new(),
            Heads {
                stable: "build-previous-good".to_string(),
                nightly: "build-previous-good".to_string(),
            },
        ));

        let heads = compute_heads(&merged, &options);

        assert_eq!(heads.nightly, options.build_tag);
        assert_eq!(heads.stable, "build-previous-good");
    }

    /// Cold start (no previous catalog) with an incoherent latest-version
    /// set: there has never been a coherent snapshot, so `stable` stays
    /// empty rather than pointing at a mismatched set (design doc §7
    /// decision 3).
    #[test]
    fn compute_heads_cold_start_incoherent_set_leaves_stable_empty() {
        let merged = vec![
            artifact_with_contracts(
                "phoxal/service-drive",
                "0.1.0",
                vec![contract_entry("publish", "y2026_1", "drive::Target", false)],
            ),
            artifact_with_contracts(
                "phoxal/service-mission",
                "0.1.0",
                vec![contract_entry(
                    "subscribe",
                    "y2026_7",
                    "drive::Target",
                    false,
                )],
            ),
        ];
        let options = base_options(Path::new("/unused"));

        let heads = compute_heads(&merged, &options);

        assert_eq!(heads.nightly, options.build_tag);
        assert_eq!(heads.stable, "");
    }

    /// `--metadata-only` (the PR gate, not a publish) has no build snapshot
    /// to point at, so both heads stay empty even over a coherent set.
    #[test]
    fn compute_heads_metadata_only_mode_leaves_both_heads_empty_even_when_coherent() {
        let merged = vec![artifact_with_contracts(
            "phoxal/service-drive",
            "0.1.0",
            vec![contract_entry("publish", "y2026_1", "drive::Target", false)],
        )];
        let mut options = base_options(Path::new("/unused"));
        options.mode = InputMode::MetadataOnly;

        let heads = compute_heads(&merged, &options);

        assert_eq!(heads.stable, "");
        assert_eq!(heads.nightly, "");
    }

    /// `latest_version_surfaces` picks the newest semver per package (not the
    /// lexicographically-last string) and skips asset-only entries (empty
    /// `contracts[]`), which cannot affect coherence.
    #[test]
    fn latest_version_surfaces_picks_newest_semver_and_skips_asset_only_entries() {
        let merged = vec![
            artifact_with_contracts(
                "phoxal/service-drive",
                "0.1.0",
                vec![contract_entry("publish", "y2026_1", "drive::Target", false)],
            ),
            // "0.9.0" < "0.10.0" as semver but > as a plain string - proves
            // the comparison is semver-aware, not lexicographic.
            artifact_with_contracts(
                "phoxal/service-drive",
                "0.10.0",
                vec![contract_entry("publish", "y2026_7", "drive::Target", false)],
            ),
            artifact_with_contracts(
                "phoxal/service-drive",
                "0.9.0",
                vec![contract_entry("publish", "y2026_3", "drive::Target", false)],
            ),
            artifact_with_contracts("phoxal/component-ddsm115", "0.1.0", Vec::new()),
        ];

        let surfaces = latest_version_surfaces(&merged);

        assert_eq!(surfaces.len(), 1);
        assert_eq!(surfaces[0].participant_id, "phoxal/service-drive");
        assert_eq!(surfaces[0].contracts.len(), 1);
        assert_eq!(surfaces[0].contracts[0].generation, "y2026_7");
    }
}
