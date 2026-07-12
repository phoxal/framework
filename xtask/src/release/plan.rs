//! `cargo xtask release plan` - the build-set decision (design doc
//! `organization/tmp/ci-release-refactor/design.md` §4.1, "Which artifacts
//! build this run").
//!
//! Artifact versions are independent: `release cut` reports only newly tagged
//! versions. By default this command plans exactly those artifacts. The
//! workflow passes `--all-artifacts` when a shared library was published, when
//! recovering a build, or when there is no coherent previous catalog to extend.
//!
//! The cut document is still consumed and validated: every newly cut release
//! must identify an official crate at its current workspace version and tag.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use serde::{Deserialize, Serialize};

use crate::release::cut::ReleaseDocument;
use crate::workspace::{ArtifactKind, OfficialArtifact, Workspace, runner_for_target};

pub(crate) const RELEASE_PLAN_SCHEMA: &str = "phoxal.release-plan/v1";

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long, value_name = "PATH")]
    pub artifact_releases_json: PathBuf,
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
    /// Plan every official artifact instead of only the versions newly emitted
    /// by `release cut`.
    #[arg(long)]
    pub all_artifacts: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ReleaseScope {
    ChangedArtifacts,
    AllArtifacts,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleasePlan {
    pub schema: String,
    pub scope: ReleaseScope,
    pub artifacts: Vec<ReleasePlanArtifact>,
    pub matrix: ReleaseMatrix,
    pub assets: Vec<ReleasePackage>,
}

/// One artifact this run's build-set includes. `package` is the
/// provider-qualified public identity (`phoxal/component-ddsm115`);
/// there is no separate `artifact_id` alongside it (docs #21). `tag` is the
/// artifact's xtask-owned version tag (`{package}-v{version}`,
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
    let releases = fs::read_to_string(&args.artifact_releases_json)
        .with_context(|| format!("failed to read {}", args.artifact_releases_json.display()))?;
    let scope = if args.all_artifacts {
        ReleaseScope::AllArtifacts
    } else {
        ReleaseScope::ChangedArtifacts
    };
    let plan = build_release_plan(&workspace, &releases, scope)?;
    write_release_plan(&args.out, &plan)?;
    if args.github_output {
        write_github_output(&plan)?;
    }
    println!(
        "release plan: {} artifacts, {} matrix rows -> {}",
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

/// Target selection uses [`OfficialArtifact::supported_target_triples`].
/// Binary work is grouped into one row per target, while components' bundles
/// are kept in [`ReleasePlan::assets`] for the workflow's single assets job.
pub(crate) fn build_release_plan(
    workspace: &Workspace,
    artifact_releases_json: &str,
    scope: ReleaseScope,
) -> Result<ReleasePlan> {
    let changed_crates = validate_release_document(workspace, artifact_releases_json)?;

    let mut artifacts: Vec<ReleasePlanArtifact> = workspace
        .official_artifacts()
        .iter()
        .filter(|artifact| {
            scope == ReleaseScope::AllArtifacts
                || artifact
                    .package_name
                    .as_ref()
                    .is_some_and(|name| changed_crates.contains(name))
        })
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
            if target == crate::workspace::ASSETS_SCOPE {
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
        scope,
        artifacts,
        matrix: ReleaseMatrix { include },
        assets,
    })
}

fn validate_release_document(workspace: &Workspace, text: &str) -> Result<BTreeSet<String>> {
    let document: ReleaseDocument =
        serde_json::from_str(text).context("artifact release output was not valid JSON")?;
    let official_by_crate = workspace
        .official_artifacts()
        .iter()
        .filter_map(|artifact| {
            artifact
                .package_name
                .as_deref()
                .map(|name| (name, artifact))
        })
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for release in document.releases {
        if !seen.insert(release.package_name.clone()) {
            bail!("artifact release output repeats {}", release.package_name);
        }
        let artifact = official_by_crate
            .get(release.package_name.as_str())
            .with_context(|| {
                format!(
                    "artifact release output names unknown crate {}",
                    release.package_name
                )
            })?;
        if release.version != artifact.version {
            bail!(
                "artifact release output says {} is v{}, but workspace discovery found v{}",
                release.package_name,
                release.version,
                artifact.version
            );
        }
        let expected_tag = artifact.release_tag();
        if release.tag != expected_tag {
            bail!(
                "artifact release output tag for {} was '{}', expected '{}'",
                release.package_name,
                release.tag,
                expected_tag
            );
        }
    }
    Ok(seen)
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
    let assets =
        serde_json::to_string(&plan.assets).context("failed to serialize asset packages")?;
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
    use std::path::PathBuf;

    use crate::release::cut::{CutRelease, ReleaseDocument};
    use crate::workspace::{PhoxalPackageMetadata, Workspace};

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

    fn releases(entries: Vec<CutRelease>) -> Result<String> {
        Ok(serde_json::to_string(&ReleaseDocument {
            releases: entries,
        })?)
    }

    #[test]
    fn empty_cut_document_still_builds_every_artifact() -> Result<()> {
        let workspace = workspace_with(vec![
            artifact(
                "phoxal-service-drive",
                ArtifactKind::Service,
                "drive",
                "0.1.0",
            ),
            artifact("phoxal-tool-router", ArtifactKind::Tool, "router", "0.1.0"),
        ]);

        let plan = build_release_plan(
            &workspace,
            &releases(Vec::new())?,
            ReleaseScope::AllArtifacts,
        )?;

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
    fn controller_only_cut_plans_only_controller() -> Result<()> {
        let workspace = workspace_with(vec![
            artifact(
                "phoxal-service-drive",
                ArtifactKind::Service,
                "drive",
                "0.19.7",
            ),
            artifact(
                "phoxal-simulator-webots-controller",
                ArtifactKind::Simulator,
                "webots-controller",
                "0.2.0",
            ),
        ]);
        let cut = releases(vec![CutRelease {
            package_name: "phoxal-simulator-webots-controller".to_string(),
            version: "0.2.0".to_string(),
            tag: "phoxal-simulator-webots-controller-v0.2.0".to_string(),
        }])?;

        let plan = build_release_plan(&workspace, &cut, ReleaseScope::ChangedArtifacts)?;

        assert_eq!(plan.scope, ReleaseScope::ChangedArtifacts);
        assert_eq!(plan.artifacts.len(), 1);
        assert_eq!(
            plan.artifacts[0].package,
            "phoxal/simulator-webots-controller"
        );
        assert_eq!(plan.artifacts[0].version, "0.2.0");
        Ok(())
    }

    #[test]
    fn all_scope_plans_unchanged_artifacts_too() -> Result<()> {
        let workspace = workspace_with(vec![
            artifact(
                "phoxal-service-drive",
                ArtifactKind::Service,
                "drive",
                "0.19.7",
            ),
            artifact("phoxal-tool-router", ArtifactKind::Tool, "router", "0.2.0"),
        ]);
        let cut = releases(vec![CutRelease {
            package_name: "phoxal-tool-router".to_string(),
            version: "0.2.0".to_string(),
            tag: "phoxal-tool-router-v0.2.0".to_string(),
        }])?;

        let plan = build_release_plan(&workspace, &cut, ReleaseScope::AllArtifacts)?;

        assert_eq!(plan.scope, ReleaseScope::AllArtifacts);
        assert_eq!(plan.artifacts.len(), 2);
        Ok(())
    }

    #[test]
    fn cut_document_version_must_match_workspace() -> Result<()> {
        let workspace = workspace_with(vec![artifact(
            "phoxal-service-drive",
            ArtifactKind::Service,
            "drive",
            "0.19.7",
        )]);
        let cut = releases(vec![CutRelease {
            package_name: "phoxal-service-drive".to_string(),
            version: "0.19.8".to_string(),
            tag: "phoxal-service-drive-v0.19.8".to_string(),
        }])?;

        let error =
            build_release_plan(&workspace, &cut, ReleaseScope::ChangedArtifacts).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("workspace discovery found v0.19.7")
        );
        Ok(())
    }

    #[test]
    fn component_plans_as_one_artifact_with_binary_and_asset_target_triples() -> Result<()> {
        // The component-as-one-crate migration (design doc §9): a `Component`
        // is one package/version carrying both its binary targets and its
        // asset bundle, so it plans as a single
        // `ReleasePlanArtifact` whose `target_triples` cover both outputs.
        let workspace = workspace_with(vec![artifact(
            "phoxal-component-ddsm115",
            ArtifactKind::Component,
            "ddsm115",
            "0.2.0",
        )]);
        let plan = build_release_plan(
            &workspace,
            &releases(Vec::new())?,
            ReleaseScope::AllArtifacts,
        )?;

        assert_eq!(plan.artifacts.len(), 1);
        assert_eq!(plan.artifacts[0].package, "phoxal/component-ddsm115");
        assert_eq!(plan.artifacts[0].version, "0.2.0");
        assert_eq!(
            plan.artifacts[0].target_triples,
            vec![
                "aarch64-apple-darwin",
                "aarch64-unknown-linux-gnu",
                "aarch64-unknown-linux-musl",
                "assets",
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

        let plan = build_release_plan(
            &workspace,
            &releases(Vec::new())?,
            ReleaseScope::AllArtifacts,
        )?;

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

    #[test]
    fn real_workspace_plans_webots_only_for_available_platforms() -> Result<()> {
        let workspace = Workspace::discover()?;
        let plan = build_release_plan(
            &workspace,
            &releases(Vec::new())?,
            ReleaseScope::AllArtifacts,
        )?;
        let expected_webots_targets = vec![
            "aarch64-apple-darwin".to_string(),
            "x86_64-unknown-linux-gnu".to_string(),
        ];

        for package in [
            "phoxal/simulator-webots-controller",
            "phoxal/simulator-webots-supervisor",
        ] {
            let artifact = plan
                .artifacts
                .iter()
                .find(|artifact| artifact.package == package)
                .with_context(|| format!("release plan is missing {package}"))?;
            assert_eq!(artifact.target_triples, expected_webots_targets);
        }

        let normal_service = plan
            .artifacts
            .iter()
            .find(|artifact| artifact.package == "phoxal/service-drive")
            .context("release plan is missing phoxal/service-drive")?;
        assert_eq!(
            normal_service.target_triples,
            vec![
                "aarch64-apple-darwin",
                "aarch64-unknown-linux-gnu",
                "aarch64-unknown-linux-musl",
                "x86_64-unknown-linux-gnu",
                "x86_64-unknown-linux-musl",
            ]
        );
        Ok(())
    }
}
