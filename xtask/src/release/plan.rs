use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::generate::catalog_artifact_id;
use crate::workspace::{ArtifactKind, OfficialArtifact, Workspace, runner_for_target};

pub(crate) const RELEASE_PLAN_SCHEMA: &str = "phoxal.release-plan/v0";

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long, value_name = "PATH")]
    pub release_plz_json: PathBuf,
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReleasePlanArtifact {
    pub package: String,
    pub version: String,
    pub tag: String,
    pub kind: ArtifactKind,
    pub artifact_id: String,
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
    pub package: String,
    pub version: String,
    pub tag: String,
    pub kind: ArtifactKind,
    pub artifact_id: String,
    pub target: String,
    pub runner: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReleasedPackage {
    package: String,
    version: String,
    tag: Option<String>,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let release_plz_text = fs::read_to_string(&args.release_plz_json)
        .with_context(|| format!("failed to read {}", args.release_plz_json.display()))?;
    let plan = build_release_plan(&workspace, &release_plz_text)?;
    write_release_plan(&args.out, &plan)?;
    if args.github_output {
        write_github_output(&plan)?;
    }
    println!(
        "release plan: {} artifact releases, {} matrix rows -> {}",
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

pub(crate) fn build_release_plan(
    workspace: &Workspace,
    release_plz_text: &str,
) -> Result<ReleasePlan> {
    let official_by_package = workspace
        .official_artifacts()
        .iter()
        .map(|artifact| (artifact.package_name.as_str(), artifact))
        .collect::<BTreeMap<_, _>>();
    let released = released_packages(release_plz_text)?;
    let mut artifacts = Vec::new();
    for release in released {
        let Some(artifact) = official_by_package.get(release.package.as_str()) else {
            continue;
        };
        if release.version != artifact.version {
            bail!(
                "release-plz JSON says {} is v{}, but workspace discovery found v{}",
                release.package,
                release.version,
                artifact.version
            );
        }
        artifacts.push(plan_artifact(artifact, release.tag.as_deref())?);
    }
    artifacts.sort_by(|left, right| left.package.cmp(&right.package));

    let mut include = Vec::new();
    for artifact in &artifacts {
        for target in &artifact.target_triples {
            include.push(ReleaseMatrixEntry {
                package: artifact.package.clone(),
                version: artifact.version.clone(),
                tag: artifact.tag.clone(),
                kind: artifact.kind,
                artifact_id: artifact.artifact_id.clone(),
                target: target.clone(),
                runner: runner_for_target(target)?.to_string(),
            });
        }
    }

    Ok(ReleasePlan {
        schema: RELEASE_PLAN_SCHEMA.to_string(),
        artifacts,
        matrix: ReleaseMatrix { include },
    })
}

fn plan_artifact(
    artifact: &OfficialArtifact,
    release_plz_tag: Option<&str>,
) -> Result<ReleasePlanArtifact> {
    let expected_tag = artifact.release_tag();
    let tag = release_plz_tag.unwrap_or(&expected_tag);
    if tag != expected_tag {
        bail!(
            "release-plz JSON tag for {} was '{}', expected '{}'",
            artifact.package_name,
            tag,
            expected_tag
        );
    }
    Ok(ReleasePlanArtifact {
        package: artifact.package_name.clone(),
        version: artifact.version.clone(),
        tag: tag.to_string(),
        kind: artifact.kind,
        artifact_id: catalog_artifact_id(artifact.kind, &artifact.id),
        target_triples: artifact.supported_target_triples(),
    })
}

fn released_packages(text: &str) -> Result<Vec<ReleasedPackage>> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: Value =
        serde_json::from_str(text).context("release-plz output was not valid JSON")?;
    let mut releases = BTreeMap::<String, ReleasedPackage>::new();
    collect_released_packages(&value, &mut releases)?;
    Ok(releases.into_values().collect())
}

fn collect_released_packages(
    value: &Value,
    releases: &mut BTreeMap<String, ReleasedPackage>,
) -> Result<()> {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_released_packages(value, releases)?;
            }
        }
        Value::Object(object) => {
            if let Some(release) = release_from_object(object) {
                match releases.get(&release.package) {
                    Some(existing) if existing != &release => bail!(
                        "release-plz JSON contains conflicting release facts for {}",
                        release.package
                    ),
                    Some(_) => {}
                    None => {
                        releases.insert(release.package.clone(), release);
                    }
                }
            }
            for value in object.values() {
                collect_released_packages(value, releases)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn release_from_object(object: &serde_json::Map<String, Value>) -> Option<ReleasedPackage> {
    let package = string_field(
        object,
        &[
            "package",
            "package_name",
            "packageName",
            "crate",
            "crate_name",
            "name",
        ],
    )?;
    let version =
        string_field(object, &["version", "new_version", "next_version"]).or_else(|| {
            version_from_tag(
                string_field(object, &["tag", "tag_name", "git_tag"])?,
                package,
            )
        })?;
    let tag = string_field(object, &["tag", "tag_name", "git_tag"]).map(str::to_string);
    Some(ReleasedPackage {
        package: package.to_string(),
        version: version.to_string(),
        tag,
    })
}

fn string_field<'a>(object: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| object.get(*key))
        .find_map(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn version_from_tag<'a>(tag: &'a str, package: &str) -> Option<&'a str> {
    tag.strip_prefix(package)?.strip_prefix("-v")
}

fn write_github_output(plan: &ReleasePlan) -> Result<()> {
    let output_path = std::env::var_os("GITHUB_OUTPUT")
        .map(PathBuf::from)
        .context("--github-output requires GITHUB_OUTPUT to be set")?;
    let matrix =
        serde_json::to_string(&plan.matrix).context("failed to serialize release matrix")?;
    let mut output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&output_path)
        .with_context(|| format!("failed to open {}", output_path.display()))?;
    writeln!(output, "released={}", !plan.artifacts.is_empty())?;
    writeln!(output, "matrix={matrix}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::workspace::{PhoxalPackageMetadata, Workspace};

    use super::*;

    fn artifact(package_name: &str, kind: ArtifactKind, id: &str) -> OfficialArtifact {
        OfficialArtifact {
            package_name: package_name.to_string(),
            kind,
            version: "0.1.0".to_string(),
            crate_dir: PathBuf::new(),
            bin_name: package_name.to_string(),
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

    #[test]
    fn release_plz_output_expands_artifact_target_matrix() -> Result<()> {
        let workspace = workspace_with(vec![
            artifact("phoxal-service-drive", ArtifactKind::Service, "drive"),
            artifact("phoxal-tool-router", ArtifactKind::Tool, "router"),
        ]);
        let plan = build_release_plan(
            &workspace,
            r#"{
  "releases": [
    { "package_name": "phoxal-service-drive", "version": "0.1.0", "tag": "phoxal-service-drive-v0.1.0" },
    { "package_name": "phoxal", "version": "0.25.0", "tag": "phoxal-v0.25.0" },
    { "package_name": "phoxal-tool-router", "version": "0.1.0", "tag": "phoxal-tool-router-v0.1.0" }
  ]
}"#,
        )?;

        assert_eq!(plan.artifacts.len(), 2);
        assert_eq!(
            plan.artifacts[0].target_triples,
            vec!["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"]
        );
        assert_eq!(
            plan.artifacts[1].target_triples,
            vec![
                "aarch64-apple-darwin",
                "aarch64-unknown-linux-gnu",
                "x86_64-unknown-linux-gnu"
            ]
        );
        assert_eq!(plan.matrix.include.len(), 5);
        Ok(())
    }

    #[test]
    fn release_plz_shape_can_be_nested_or_array_based() -> Result<()> {
        let workspace = workspace_with(vec![artifact(
            "phoxal-driver-ddsm115",
            ArtifactKind::Driver,
            "ddsm115",
        )]);
        let plan = build_release_plan(
            &workspace,
            r#"[
  { "name": "phoxal-driver-ddsm115", "version": "0.1.0", "git_tag": "phoxal-driver-ddsm115-v0.1.0" }
]"#,
        )?;

        assert_eq!(plan.artifacts.len(), 1);
        assert_eq!(plan.artifacts[0].tag, "phoxal-driver-ddsm115-v0.1.0");
        Ok(())
    }

    #[test]
    fn mismatched_release_version_fails_closed() {
        let workspace = workspace_with(vec![artifact(
            "phoxal-service-drive",
            ArtifactKind::Service,
            "drive",
        )]);
        let err = build_release_plan(
            &workspace,
            r#"{ "package_name": "phoxal-service-drive", "version": "0.2.0" }"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("workspace discovery found"));
    }
}
