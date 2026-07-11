//! `cargo xtask release plan` - the build-set decision (design doc
//! `organization/tmp/ci-release-refactor/design.md` §4.1, "Which artifacts
//! build this run").
//!
//! release-plz (`release-plz.toml`) now owns *all* crate versioning -
//! libraries and binaries alike, the latter via `git_only` - so this command
//! no longer reads release-plz's JSON output to learn which packages were
//! released. Versioning and building are decoupled: release-plz decides
//! versions on the release-PR merge; this command turns the complete official
//! artifact set into the build matrix.
//!
//! `release_always = true` requires every official artifact to bump on every
//! release train. If any current `(package, version)` is already present in the
//! previous catalog, planning fails: silently excluding it would make the
//! workspace at HEAD differ from the complete package set users download.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use serde::{Deserialize, Serialize};

use phoxal::catalog::Catalog;

use crate::catalog::verify::verify_catalog_path;
use crate::workspace::{ArtifactKind, OfficialArtifact, Workspace, runner_for_target};

pub(crate) const RELEASE_PLAN_SCHEMA: &str = "phoxal.release-plan/v0";

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// The current "latest" `phoxal.catalog/v0` catalog, downloaded by the
    /// workflow before this step. Absent means cold start (§4.3): every
    /// discovered artifact is included in the plan.
    #[arg(long, value_name = "PATH")]
    pub previous_catalog: Option<PathBuf>,
    #[arg(
        long,
        value_name = "PATH",
        default_value = "target/xtask/release-plan.json"
    )]
    pub out: PathBuf,
    /// Also write `released` and `matrix` outputs to the GitHub Actions output
    /// file named by GITHUB_OUTPUT.
    #[arg(long)]
    pub github_output: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleasePlan {
    pub schema: String,
    pub artifacts: Vec<ReleasePlanArtifact>,
    pub matrix: ReleaseMatrix,
    pub assets: Vec<ReleasePackage>,
}

/// One artifact this run's build-set includes. `package` is the
/// provider-qualified public identity (`phoxal/component-ddsm115`);
/// there is no separate `artifact_id` alongside it (docs #21). `tag` is the
/// artifact's release-plz-owned version tag (`{package}-v{version}`,
/// informational only - the actual upload target is this run's `build-*`
/// release, decided by the workflow, not this plan).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleasePlanArtifact {
    pub package: String,
    pub version: String,
    pub tag: String,
    pub kind: ArtifactKind,
    pub target_triples: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseMatrix {
    pub include: Vec<ReleaseMatrixEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleaseMatrixEntry {
    pub target: String,
    pub runner: String,
    pub packages: Vec<ReleasePackage>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleasePackage {
    pub package: String,
    pub version: String,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let previous_catalog = args
        .previous_catalog
        .as_deref()
        .map(verify_catalog_path)
        .transpose()?;
    let plan = build_release_plan(&workspace, previous_catalog.as_ref())?;
    write_release_plan(&args.out, &plan)?;
    if args.github_output {
        write_github_output(&plan)?;
    }
    println!(
        "release plan: {} artifact release(s), {} matrix rows -> {}",
        plan.artifacts.len(),
        plan.matrix.include.len(),
        args.out.display()
    );
    Ok(())
}

pub(crate) fn load_release_plan(path: &Path) -> Result<ReleasePlan> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let plan: ReleasePlan = serde_json::from_str(&text).with_context(|| {
        format!(
            "{} is not a valid release plan JSON document",
            path.display()
        )
    })?;
    if plan.schema != RELEASE_PLAN_SCHEMA {
        bail!(
            "{} has release-plan schema '{}', expected '{}'",
            path.display(),
            plan.schema,
            RELEASE_PLAN_SCHEMA
        );
    }
    Ok(plan)
}

pub(crate) fn write_release_plan(path: &Path, plan: &ReleasePlan) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut json =
        serde_json::to_string_pretty(plan).context("failed to serialize release plan")?;
    json.push('\n');
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}

/// Builds the complete official-artifact plan. A current `(package, version)`
/// already present in `previous_catalog` is a release invariant violation,
/// because `release_always = true` must bump and repackage the whole set.
///
/// Target selection uses [`OfficialArtifact::supported_target_triples`].
/// Binary work is grouped into one row per target, while components' additional
/// target-independent bundles are kept in [`ReleasePlan::assets`] for the
/// workflow's single assets job.
pub(crate) fn build_release_plan(
    workspace: &Workspace,
    previous_catalog: Option<&Catalog>,
) -> Result<ReleasePlan> {
    let already_published: BTreeSet<(&str, &str)> = previous_catalog
        .map(|catalog| {
            catalog
                .artifacts
                .iter()
                .map(|entry| (entry.package.as_str(), entry.version.as_str()))
                .collect()
        })
        .unwrap_or_default();

    let offenders: Vec<String> = workspace
        .official_artifacts()
        .iter()
        .filter(|artifact| {
            already_published.contains(&(artifact.package.as_str(), artifact.version.as_str()))
        })
        .map(|artifact| format!("{} v{}", artifact.package, artifact.version))
        .collect();
    if !offenders.is_empty() {
        bail!(
            "release invariant violated: current official artifact version(s) already exist in \
             the previous catalog: {}. With release_always = true every official artifact must \
             bump and be repackaged on every release train; check release-plz.toml coverage",
            offenders.join(", ")
        );
    }

    let mut artifacts: Vec<ReleasePlanArtifact> = workspace
        .official_artifacts()
        .iter()
        .map(plan_artifact)
        .collect();
    artifacts.sort_by(|left, right| left.package.cmp(&right.package));

    let mut packages_by_target: BTreeMap<String, Vec<ReleasePackage>> = BTreeMap::new();
    let mut assets = Vec::new();
    for artifact in &artifacts {
        for target in &artifact.target_triples {
            let package = ReleasePackage {
                package: artifact.package.clone(),
                version: artifact.version.clone(),
            };
            if target == crate::workspace::TARGET_INDEPENDENT_SCOPE {
                assets.push(package);
            } else {
                packages_by_target
                    .entry(target.clone())
                    .or_default()
                    .push(package);
            }
        }
    }
    let include = packages_by_target
        .into_iter()
        .map(|(target, packages)| {
            Ok(ReleaseMatrixEntry {
                runner: runner_for_target(&target)?.to_string(),
                target,
                packages,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(ReleasePlan {
        schema: RELEASE_PLAN_SCHEMA.to_string(),
        artifacts,
        matrix: ReleaseMatrix { include },
        assets,
    })
}

fn plan_artifact(artifact: &OfficialArtifact) -> ReleasePlanArtifact {
    ReleasePlanArtifact {
        package: artifact.package.clone(),
        version: artifact.version.clone(),
        tag: artifact.release_tag(),
        kind: artifact.kind,
        target_triples: artifact.supported_target_triples(),
    }
}

fn write_github_output(plan: &ReleasePlan) -> Result<()> {
    let output_path = std::env::var_os("GITHUB_OUTPUT")
        .map(PathBuf::from)
        .context("--github-output requires GITHUB_OUTPUT to be set")?;
    let matrix =
        serde_json::to_string(&plan.matrix).context("failed to serialize release matrix")?;
    let assets = serde_json::to_string(&plan.assets)
        .context("failed to serialize target-independent release packages")?;
    let mut output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&output_path)
        .with_context(|| format!("failed to open {}", output_path.display()))?;
    writeln!(output, "released={}", !plan.artifacts.is_empty())?;
    writeln!(output, "matrix={matrix}")?;
    writeln!(output, "assets={assets}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use crate::workspace::{PhoxalPackageMetadata, Workspace};
    use phoxal::catalog::{Artifact as CatalogArtifact, Blob, BuildProvenance, Heads};

    use super::*;

    fn artifact(
        package_name: &str,
        kind: ArtifactKind,
        id: &str,
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
            metadata: PhoxalPackageMetadata::default(),
        }
    }

    fn workspace_with(artifacts: Vec<OfficialArtifact>) -> Workspace {
        Workspace::from_parts_for_tests(
            PathBuf::from("/repo"),
            PathBuf::from("/repo/target"),
            artifacts,
        )
    }

    fn fixture_build_provenance() -> BuildProvenance {
        BuildProvenance {
            tag: "build-20260701-0001180".to_string(),
            run_number: 1180,
            run_id: 1,
            commit: "c".to_string(),
            created_at: "2026-07-01T00:00:00Z".to_string(),
        }
    }

    fn catalog_entry(package: &str, version: &str) -> CatalogArtifact {
        let mut targets = BTreeMap::new();
        targets.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            Blob {
                url: "https://example.invalid/a.tar.zst".to_string(),
                sha256: "a".repeat(64),
                size: 1,
            },
        );
        CatalogArtifact {
            package: package.to_string(),
            version: version.to_string(),
            targets,
            assets: None,
        }
    }

    #[test]
    fn cold_start_with_no_previous_catalog_builds_every_artifact() -> Result<()> {
        let workspace = workspace_with(vec![
            artifact(
                "phoxal-service-drive",
                ArtifactKind::Service,
                "drive",
                "0.1.0",
            ),
            artifact("phoxal-tool-router", ArtifactKind::Tool, "router", "0.1.0"),
        ]);

        let plan = build_release_plan(&workspace, None)?;

        let uniform_five = vec![
            "aarch64-apple-darwin".to_string(),
            "aarch64-unknown-linux-gnu".to_string(),
            "aarch64-unknown-linux-musl".to_string(),
            "x86_64-unknown-linux-gnu".to_string(),
            "x86_64-unknown-linux-musl".to_string(),
        ];

        assert_eq!(plan.artifacts.len(), 2);
        assert_eq!(plan.artifacts[0].target_triples, uniform_five);
        assert_eq!(plan.artifacts[1].target_triples, uniform_five);
        assert_eq!(plan.matrix.include.len(), 5);
        for row in &plan.matrix.include {
            assert_eq!(row.packages.len(), 2);
            assert_eq!(row.packages[0].package, "phoxal/service-drive");
            assert_eq!(row.packages[1].package, "phoxal/tool-router");
        }
        assert!(plan.assets.is_empty());
        Ok(())
    }

    #[test]
    fn already_catalogued_current_version_fails_the_release_invariant() -> Result<()> {
        let workspace = workspace_with(vec![artifact(
            "phoxal-service-drive",
            ArtifactKind::Service,
            "drive",
            "0.19.7",
        )]);
        let previous = Catalog::new(
            fixture_build_provenance(),
            vec![catalog_entry("phoxal/service-drive", "0.19.7")],
            Heads::empty(),
        );

        let err = build_release_plan(&workspace, Some(&previous)).unwrap_err();

        assert!(err.to_string().contains("release invariant violated"));
        assert!(err.to_string().contains("phoxal/service-drive v0.19.7"));
        assert!(err.to_string().contains("release-plz.toml coverage"));
        Ok(())
    }

    #[test]
    fn a_new_version_not_yet_in_the_catalog_is_included() -> Result<()> {
        let workspace = workspace_with(vec![artifact(
            "phoxal-service-drive",
            ArtifactKind::Service,
            "drive",
            "0.19.8",
        )]);
        // Previous catalog only knows about the OLDER version - the workspace's
        // current version (0.19.8) is new and must be built.
        let previous = Catalog::new(
            fixture_build_provenance(),
            vec![catalog_entry("phoxal/service-drive", "0.19.7")],
            Heads::empty(),
        );

        let plan = build_release_plan(&workspace, Some(&previous))?;

        assert_eq!(plan.artifacts.len(), 1);
        assert_eq!(plan.artifacts[0].package, "phoxal/service-drive");
        assert_eq!(plan.artifacts[0].version, "0.19.8");
        assert_eq!(plan.artifacts[0].tag, "phoxal-service-drive-v0.19.8");
        Ok(())
    }

    #[test]
    fn one_unbumped_artifact_blocks_a_mixed_release_train() -> Result<()> {
        let workspace = workspace_with(vec![
            artifact(
                "phoxal-service-drive",
                ArtifactKind::Service,
                "drive",
                "0.19.7",
            ),
            artifact("phoxal-tool-router", ArtifactKind::Tool, "router", "0.2.0"),
        ]);
        let previous = Catalog::new(
            fixture_build_provenance(),
            vec![catalog_entry("phoxal/service-drive", "0.19.7")],
            Heads::empty(),
        );

        let err = build_release_plan(&workspace, Some(&previous)).unwrap_err();

        assert!(err.to_string().contains("phoxal/service-drive v0.19.7"));
        assert!(!err.to_string().contains("phoxal/tool-router v0.2.0"));
        Ok(())
    }

    #[test]
    fn component_plans_as_one_artifact_with_binary_and_asset_target_triples() -> Result<()> {
        // The component-as-one-crate migration (design doc §9): a `Component`
        // is one package/version carrying both its binary targets and its
        // target-independent asset bundle, so it plans as a single
        // `ReleasePlanArtifact` whose `target_triples` cover both outputs.
        let workspace = workspace_with(vec![artifact(
            "phoxal-component-ddsm115",
            ArtifactKind::Component,
            "ddsm115",
            "0.2.0",
        )]);
        let previous = Catalog::new(
            fixture_build_provenance(),
            vec![catalog_entry("phoxal/component-ddsm115", "0.1.0")],
            Heads::empty(),
        );

        let plan = build_release_plan(&workspace, Some(&previous))?;

        assert_eq!(plan.artifacts.len(), 1);
        assert_eq!(plan.artifacts[0].package, "phoxal/component-ddsm115");
        assert_eq!(plan.artifacts[0].version, "0.2.0");
        assert_eq!(
            plan.artifacts[0].target_triples,
            vec![
                "aarch64-apple-darwin",
                "aarch64-unknown-linux-gnu",
                "aarch64-unknown-linux-musl",
                "target-independent",
                "x86_64-unknown-linux-gnu",
                "x86_64-unknown-linux-musl"
            ]
        );
        assert_eq!(plan.matrix.include.len(), 5);
        assert_eq!(plan.assets.len(), 1);
        assert_eq!(plan.assets[0].package, "phoxal/component-ddsm115");
        Ok(())
    }

    #[test]
    fn matrix_rows_only_contain_packages_supporting_the_target() -> Result<()> {
        let service = artifact(
            "phoxal-service-drive",
            ArtifactKind::Service,
            "drive",
            "0.1.0",
        );
        let mut joypad = artifact("phoxal-tool-joypad", ArtifactKind::Tool, "joypad", "0.1.0");
        joypad.metadata.unsupported_targets = vec!["*-musl".to_string()];
        let workspace = workspace_with(vec![service, joypad]);

        let plan = build_release_plan(&workspace, None)?;

        assert_eq!(plan.matrix.include.len(), 5);
        for row in &plan.matrix.include {
            let packages = row
                .packages
                .iter()
                .map(|package| package.package.as_str())
                .collect::<Vec<_>>();
            assert_eq!(packages.first(), Some(&"phoxal/service-drive"));
            if row.target.ends_with("-musl") {
                assert_eq!(packages, vec!["phoxal/service-drive"]);
            } else {
                assert_eq!(packages, vec!["phoxal/service-drive", "phoxal/tool-joypad"]);
            }
        }
        Ok(())
    }
}
