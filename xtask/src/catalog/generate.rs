//! `cargo xtask catalog generate` - assemble-from-facts, never build.
//!
//! Design doc `organization/tmp/ci-release-refactor/design.md` §4.2: this
//! command never invokes `cargo build`/`cargo auditable build` and never
//! executes a binary. Everything is read straight off facts already on disk:
//! the packaged tarballs + checksums in `--package-dir` (produced by `release
//! package`) and, for publish-time coherence, the metadata sections embedded
//! in every target binary.
//!
//! The merge is keyed by `(package, version)` (§4.2): start from
//! `--previous-catalog`'s full `artifacts[]` (empty if absent - cold start,
//! §4.3), then upsert one fresh entry per `(package, version)` this run has
//! facts for in `--package-dir`. Older versions are retained; a rebuilt current
//! `(package, version)` replaces its prior location with this wave's immutable
//! `build-*` URL. Re-running generate against the same `--package-dir` is
//! idempotent.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use phoxal::catalog::{Artifact as CatalogArtifact, Blob, BuildProvenance, Catalog, Heads};
use phoxal::check::{ParticipantContractSurface, check_coherence};

use crate::release::package::{self, PackagedOutput};
use crate::release::plan::{ReleasePlan, ReleaseScope, load_release_plan};
use crate::workspace::{ASSETS_SCOPE, OfficialArtifact, Workspace, require_nonempty_artifacts};

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
    /// Generate the PR-gate catalog shape without release builds or tarballs.
    /// Entries carry package/version only; both heads stay empty because no
    /// publishable build snapshot exists.
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

    let catalog = build_catalog(artifacts, &options, build)?;
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
/// coherence-gate design doc §4 `heads` computed from this run's packaged
/// binaries.
pub(crate) fn build_catalog(
    artifacts: &[OfficialArtifact],
    options: &GenerateOptions,
    build: BuildProvenance,
) -> Result<Catalog> {
    if let Some(plan) = &options.release_plan {
        validate_release_plan_facts(plan, artifacts, options)?;
    }
    let merged = merge_artifacts(artifacts, options)?;
    let heads = compute_heads(artifacts, options)?;
    Ok(Catalog::new(build, merged, heads))
}

// ---------------------------------------------------------------------------
// Heads (coherence-gate design doc §4: whole-set snapshot pointers)
// ---------------------------------------------------------------------------

/// Computes the coherence-gate design doc §4 `heads`: `nightly` is always
/// this run's build tag; `stable` is this run's build tag iff the
/// complete official participant set extracted from this run's packaged
/// binaries passes `phoxal::check::check_coherence`,
/// else it is carried forward from `options.previous_catalog`'s `stable`
/// (empty if there is no previous catalog or its `stable` was itself empty -
/// cold start with an incoherent set has no coherent snapshot to point at,
/// design doc §4.3/§7 decision 3).
///
/// `--metadata-only` mode (the PR gate, not a publish - see the module docs)
/// has no build snapshot to point at, so both heads stay empty regardless of
/// coherence.
fn compute_heads(artifacts: &[OfficialArtifact], options: &GenerateOptions) -> Result<Heads> {
    if options.mode == InputMode::MetadataOnly {
        return Ok(Heads::empty());
    }

    if options
        .release_plan
        .as_ref()
        .is_some_and(|plan| plan.scope == ReleaseScope::ChangedArtifacts)
    {
        validate_changed_artifact_metadata(artifacts, options)?;
        return heads_for_changed_artifacts(options);
    }

    let mut surfaces = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        let triples = binary_target_triples(artifact);
        let meta = package::extract_metadata_from_packaged(
            artifact,
            &options.package_dir,
            &triples,
        )
        .with_context(|| {
            format!(
                "failed to extract packaged metadata for {} v{} while computing catalog heads",
                artifact.package, artifact.version
            )
        })?;
        surfaces.push(ParticipantContractSurface {
            participant_id: artifact.package.clone(),
            contracts: meta.contracts,
        });
    }

    Ok(heads_from_surfaces(&surfaces, options))
}

/// A changed-artifact build extends the preceding coherent full set. The PR
/// gate has already checked the complete source-level participant set; here we
/// still extract every changed binary's metadata to enforce cross-target
/// equality. A cold start or a previous non-coherent latest build must never
/// enter this path: the workflow requests an all-artifacts plan instead.
fn validate_changed_artifact_metadata(
    artifacts: &[OfficialArtifact],
    options: &GenerateOptions,
) -> Result<()> {
    let plan = options
        .release_plan
        .as_ref()
        .context("changed-artifact catalog generation requires a release plan")?;
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
        package::extract_metadata_from_packaged(
            artifact,
            &options.package_dir,
            &binary_target_triples(artifact),
        )
        .with_context(|| {
            format!(
                "failed to validate changed packaged metadata for {} v{}",
                artifact.package, artifact.version
            )
        })?;
    }
    Ok(())
}

fn heads_for_changed_artifacts(options: &GenerateOptions) -> Result<Heads> {
    let previous = options.previous_catalog.as_ref().context(
        "changed-artifact release requires a previous coherent latest catalog; rebuild all artifacts",
    )?;
    if previous.heads.stable.is_empty() || previous.heads.stable != previous.build.tag {
        bail!(
            "changed-artifact release cannot extend previous build {} because its stable head is '{}'; rebuild all artifacts",
            previous.build.tag,
            previous.heads.stable
        );
    }
    Ok(Heads {
        stable: options.build_tag.clone(),
        nightly: options.build_tag.clone(),
    })
}

fn heads_from_surfaces(
    surfaces: &[ParticipantContractSurface],
    options: &GenerateOptions,
) -> Heads {
    let report = check_coherence(surfaces);
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
                "warning: the packaged official artifact set is incoherent and no previous \
                 coherent snapshot exists - heads.stable stays empty until a coherent snapshot \
                 is published (coherence-gate design doc §4)"
            );
        }
        previous_stable
    };

    Heads { stable, nightly }
}

/// A release plan names every package/target this run was supposed to build.
/// Every planned tarball must be present before catalog assembly so the
/// cross-target metadata gate observes the complete released target set.
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
        for triple in &planned.target_triples {
            let tarball = packaged_tarball_path(artifact, &options.package_dir, triple);
            if !tarball.is_file() {
                bail!(
                    "release plan expected {} v{} target {} to be packaged this run, but no \
                     packaged output was found at {}",
                    artifact.package,
                    artifact.version,
                    triple,
                    tarball.display()
                );
            }
        }
    }
    Ok(())
}

fn merge_artifacts(
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
        let entry = build_entry(artifact, options)?;
        merged.insert((artifact.package.clone(), artifact.version.clone()), entry);
    }

    Ok(merged.into_values().collect())
}

/// Whether `artifact`'s *current* version has facts on disk this run: in
/// `MetadataOnly` mode every artifact always does (the entry is schema-only);
/// in `PackageOutputs` mode, whether at least one packaged tarball
/// (binary target or, for a [`ArtifactKind::Component`], the asset bundle) is
/// present in `--package-dir`.
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
/// entry except [`ASSETS_SCOPE`] (a [`ArtifactKind::Component`]'s
/// asset-bundle output is never a binary and has no `#[derive(phoxal::Api)]`
/// section to extract).
fn binary_target_triples(artifact: &OfficialArtifact) -> Vec<String> {
    artifact
        .supported_target_triples()
        .into_iter()
        .filter(|triple| triple != ASSETS_SCOPE)
        .collect()
}

fn build_entry(artifact: &OfficialArtifact, options: &GenerateOptions) -> Result<CatalogArtifact> {
    let mut targets = BTreeMap::new();
    let mut assets = None;

    if options.mode == InputMode::PackageOutputs {
        for triple in artifact.supported_target_triples() {
            if !packaged_tarball_path(artifact, &options.package_dir, &triple).is_file() {
                continue;
            }
            let output = package::read_packaged_output(artifact, &options.package_dir, &triple)?;
            let blob = blob_from_output(options, &output);
            if triple == ASSETS_SCOPE {
                assets = Some(blob);
            } else {
                targets.insert(triple, blob);
            }
        }
    }

    Ok(CatalogArtifact {
        package: artifact.package.clone(),
        version: artifact.version.clone(),
        targets,
        assets,
    })
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

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;

    use phoxal::participant::metadata::ParticipantMetaContract;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::workspace::ArtifactKind;

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

    #[test]
    fn cold_start_assembles_from_facts_alone() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let assets = component_artifact("ddsm115", "0.1.0");
        write_packaged_fixture(temp.path(), &assets, ASSETS_SCOPE)?;

        let options = base_options(temp.path());
        let merged = merge_artifacts(std::slice::from_ref(&assets), &options)?;

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].package, "phoxal/component-ddsm115");
        assert_eq!(merged[0].version, "0.1.0");
        assert!(merged[0].targets.is_empty());
        let blob = merged[0].assets.as_ref().expect("assets blob present");
        assert_eq!(
            blob.url,
            "https://github.com/phoxal/framework/releases/download/build-20260708-0001234/\
             phoxal-component-ddsm115-v0.1.0.tar.zst"
        );
        assert_eq!(blob.size, b"fake tarball".len() as u64);
        Ok(())
    }

    #[test]
    fn merge_appends_new_versions_without_dropping_old_ones() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let assets_v2 = component_artifact("ddsm115", "0.2.0");
        write_packaged_fixture(temp.path(), &assets_v2, ASSETS_SCOPE)?;

        let previous = Catalog::new(
            fixture_build(),
            vec![CatalogArtifact {
                package: "phoxal/component-ddsm115".to_string(),
                version: "0.1.0".to_string(),
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

        let mut options = base_options(temp.path());
        options.previous_catalog = Some(previous);
        let merged = merge_artifacts(std::slice::from_ref(&assets_v2), &options)?;

        assert_eq!(merged.len(), 2);
        let versions: Vec<&str> = merged.iter().map(|entry| entry.version.as_str()).collect();
        assert_eq!(versions, vec!["0.1.0", "0.2.0"]);
        assert_eq!(
            merged[0].assets.as_ref().unwrap().url,
            "https://github.com/phoxal/framework/releases/download/build-old/a.tar.zst"
        );
        Ok(())
    }

    #[test]
    fn merge_refreshes_a_rebuilt_unchanged_version() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let assets = component_artifact("ddsm115", "0.1.0");
        write_packaged_fixture(temp.path(), &assets, ASSETS_SCOPE)?;
        let previous = Catalog::new(
            fixture_build(),
            vec![CatalogArtifact {
                package: assets.package.clone(),
                version: assets.version.clone(),
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

        let mut options = base_options(temp.path());
        options.previous_catalog = Some(previous);
        let merged = merge_artifacts(std::slice::from_ref(&assets), &options)?;

        assert_eq!(merged.len(), 1);
        assert!(
            merged[0]
                .assets
                .as_ref()
                .unwrap()
                .url
                .contains("build-20260708-0001234")
        );
        Ok(())
    }

    #[test]
    fn regenerating_from_the_same_facts_is_idempotent() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let assets = component_artifact("ddsm115", "0.1.0");
        write_packaged_fixture(temp.path(), &assets, ASSETS_SCOPE)?;

        let options = base_options(temp.path());
        let first = merge_artifacts(std::slice::from_ref(&assets), &options)?;
        let second = merge_artifacts(std::slice::from_ref(&assets), &options)?;
        assert_eq!(first, second);
        Ok(())
    }

    #[test]
    fn release_plan_facts_are_required_when_present() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let assets = component_artifact("ddsm115", "0.1.0");
        let plan = ReleasePlan {
            schema: crate::release::plan::RELEASE_PLAN_SCHEMA.to_string(),
            scope: ReleaseScope::AllArtifacts,
            artifacts: vec![crate::release::plan::ReleasePlanArtifact {
                package: assets.package.clone(),
                version: assets.version.clone(),
                tag: assets.release_tag(),
                kind: ArtifactKind::Component,
                target_triples: vec![ASSETS_SCOPE.to_string()],
            }],
            matrix: crate::release::plan::ReleaseMatrix {
                include: Vec::new(),
            },
            assets: Vec::new(),
        };

        let options = base_options(temp.path());
        let err = build_catalog(
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

    fn fixture_build() -> BuildProvenance {
        BuildProvenance {
            tag: "build-old".to_string(),
            run_number: 1,
            run_id: 1,
            commit: "old".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn meta_contract(role: &str, version: &str, contract: &str) -> ParticipantMetaContract {
        ParticipantMetaContract {
            role: role.to_string(),
            version: version.to_string(),
            contract: contract.to_string(),
            external: false,
        }
    }

    fn surface(
        package: &str,
        contracts: Vec<ParticipantMetaContract>,
    ) -> ParticipantContractSurface {
        ParticipantContractSurface {
            participant_id: package.to_string(),
            contracts,
        }
    }

    #[test]
    fn coherent_packaged_surfaces_point_stable_and_nightly_at_this_run() {
        let surfaces = vec![
            surface(
                "phoxal/service-drive",
                vec![meta_contract("publish", "v1", "drive::Target")],
            ),
            surface(
                "phoxal/service-mission",
                vec![meta_contract("subscribe", "v1", "drive::Target")],
            ),
        ];
        let options = base_options(Path::new("/unused"));

        let heads = heads_from_surfaces(&surfaces, &options);

        assert_eq!(heads.nightly, options.build_tag);
        assert_eq!(heads.stable, options.build_tag);
    }

    #[test]
    fn incoherent_packaged_surfaces_carry_forward_previous_stable() {
        let surfaces = vec![
            surface(
                "phoxal/service-drive",
                vec![meta_contract("publish", "v1", "drive::Target")],
            ),
            surface(
                "phoxal/service-mission",
                vec![meta_contract("subscribe", "v2", "drive::Target")],
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

        let heads = heads_from_surfaces(&surfaces, &options);

        assert_eq!(heads.nightly, options.build_tag);
        assert_eq!(heads.stable, "build-previous-good");
    }

    #[test]
    fn incoherent_packaged_surfaces_on_cold_start_leave_stable_empty() {
        let surfaces = vec![
            surface(
                "phoxal/service-drive",
                vec![meta_contract("publish", "v1", "drive::Target")],
            ),
            surface(
                "phoxal/service-mission",
                vec![meta_contract("subscribe", "v2", "drive::Target")],
            ),
        ];
        let options = base_options(Path::new("/unused"));

        let heads = heads_from_surfaces(&surfaces, &options);

        assert_eq!(heads.nightly, options.build_tag);
        assert_eq!(heads.stable, "");
    }

    #[test]
    fn metadata_only_heads_stay_empty_without_reading_packaged_binaries() -> Result<()> {
        let mut options = base_options(Path::new("/does-not-exist"));
        options.mode = InputMode::MetadataOnly;

        let heads = compute_heads(&[], &options)?;

        assert_eq!(heads, Heads::empty());
        Ok(())
    }

    #[test]
    fn changed_artifacts_advance_heads_from_a_coherent_latest_catalog() -> Result<()> {
        let mut options = base_options(Path::new("/unused"));
        let previous_build = fixture_build();
        options.previous_catalog = Some(Catalog::new(
            previous_build.clone(),
            Vec::new(),
            Heads {
                stable: previous_build.tag.clone(),
                nightly: previous_build.tag,
            },
        ));

        let heads = heads_for_changed_artifacts(&options)?;

        assert_eq!(heads.stable, options.build_tag);
        assert_eq!(heads.nightly, options.build_tag);
        Ok(())
    }

    #[test]
    fn changed_artifacts_reject_a_non_coherent_previous_latest_catalog() {
        let mut options = base_options(Path::new("/unused"));
        options.previous_catalog = Some(Catalog::new(
            fixture_build(),
            Vec::new(),
            Heads {
                stable: "build-previous-good".to_string(),
                nightly: "build-old".to_string(),
            },
        ));

        let error = heads_for_changed_artifacts(&options).unwrap_err();

        assert!(error.to_string().contains("rebuild all artifacts"));
    }
}
