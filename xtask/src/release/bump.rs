use std::collections::BTreeSet;
use std::fs;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use semver::Version;
use toml_edit::{DocumentMut, value};

use crate::api::manifest::{self, ContractDiff};
use crate::release::package;
use crate::workspace::{OfficialArtifact, Workspace, require_nonempty_artifacts};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(
        value_name = "PACKAGE",
        required_unless_present_any = ["affected", "all"],
        conflicts_with_all = ["affected", "all"]
    )]
    pub package: Option<String>,
    /// Bump every artifact that uses a contract changed by the latest API
    /// generation relative to its prior generation.
    #[arg(long, conflicts_with = "all")]
    pub affected: bool,
    /// Bump every discovered artifact crate.
    #[arg(long, conflicts_with = "affected")]
    pub all: bool,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let selected = select_artifacts(&workspace, &args)?;
    if selected.is_empty() {
        println!("release bump: no artifact version bumps needed");
        return Ok(());
    }
    require_nonempty_artifacts(&selected)?;

    for artifact in selected {
        let manifest_path = artifact.crate_dir.join("Cargo.toml");
        let (from, to) = bump_manifest_version(&manifest_path)
            .with_context(|| format!("failed to bump {}", manifest_path.display()))?;
        println!("bumped {}: {} -> {}", artifact.package_name, from, to);
    }

    Ok(())
}

fn select_artifacts(workspace: &Workspace, args: &Args) -> Result<Vec<OfficialArtifact>> {
    if args.all {
        return Ok(workspace.official_artifacts().to_vec());
    }
    if args.affected {
        return affected_artifacts(workspace);
    }

    let package_name = args
        .package
        .as_deref()
        .context("package is required unless --affected or --all is present")?;
    Ok(vec![workspace.official_artifact(package_name)?.clone()])
}

fn affected_artifacts(workspace: &Workspace) -> Result<Vec<OfficialArtifact>> {
    let api_manifest = manifest::load_from_workspace(workspace)?;
    let target_generation = api_manifest
        .last()
        .with_context(|| "API manifest contains no generations")?;
    let base_generation = manifest::base_generation_name(&api_manifest, &target_generation.name)?;
    let changed_contracts =
        manifest::diff_contracts(&api_manifest, &base_generation, &target_generation.name)?;
    let changed_keys = changed_contract_keys(&changed_contracts);
    if changed_keys.is_empty() {
        return Ok(Vec::new());
    }

    let mut selected = Vec::new();
    for artifact in workspace.official_artifacts() {
        let stdout = package::emit_apis_from_cargo_run(workspace.root(), artifact)?;
        let metadata = package::parse_emit_apis_json(&stdout, &artifact.id, artifact.kind)
            .with_context(|| {
                format!("invalid emit-apis metadata from {}", artifact.package_name)
            })?;
        let uses_changed_contract = metadata.required_contracts.iter().any(|contract| {
            let Some(family) = contract.family.as_deref() else {
                return false;
            };
            let Some(topic) = contract.topic.as_deref() else {
                return false;
            };
            changed_keys.contains(&(family.to_string(), topic.to_string()))
        });
        if uses_changed_contract {
            selected.push(artifact.clone());
        }
    }

    Ok(selected)
}

fn changed_contract_keys(changed_contracts: &[ContractDiff]) -> BTreeSet<(String, String)> {
    changed_contracts
        .iter()
        .map(|contract| (contract.family.clone(), contract.topic.clone()))
        .collect()
}

fn bump_manifest_version(manifest_path: &std::path::Path) -> Result<(String, String)> {
    let text = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let mut document = text
        .parse::<DocumentMut>()
        .with_context(|| format!("{} is not valid TOML", manifest_path.display()))?;
    let package = document["package"]
        .as_table_mut()
        .with_context(|| format!("{} has no [package] table", manifest_path.display()))?;
    let current = package
        .get("version")
        .and_then(|item| item.as_str())
        .with_context(|| format!("{} [package] version is missing", manifest_path.display()))?
        .to_string();
    let bumped = bump_patch_version(&current)?;
    package["version"] = value(bumped.clone());
    fs::write(manifest_path, document.to_string())
        .with_context(|| format!("failed to write {}", manifest_path.display()))?;
    Ok((current, bumped))
}

fn bump_patch_version(version: &str) -> Result<String> {
    let mut version = Version::parse(version).with_context(|| {
        format!("artifact version '{version}' is not valid semver and cannot be bumped")
    })?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        bail!("artifact version '{version}' has pre-release or build metadata; bump it manually");
    }
    version.patch += 1;
    Ok(version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_bump_increments_patch() -> Result<()> {
        assert_eq!(bump_patch_version("0.19.1")?, "0.19.2");
        assert_eq!(bump_patch_version("1.2.3")?, "1.2.4");
        Ok(())
    }

    #[test]
    fn patch_bump_rejects_prerelease() {
        let err = bump_patch_version("0.2.0-alpha.1").unwrap_err();
        assert!(err.to_string().contains("pre-release"));
    }

    #[test]
    fn changed_keys_include_family_and_topic() {
        let keys = changed_contract_keys(&[ContractDiff {
            kind: crate::api::manifest::ContractDiffKind::Changed,
            family: "battery::State".to_string(),
            topic: "battery/state".to_string(),
            from_schema_id: Some("aaaaaaaaaaaaaaaa".to_string()),
            to_schema_id: Some("bbbbbbbbbbbbbbbb".to_string()),
        }]);

        assert!(keys.contains(&("battery::State".to_string(), "battery/state".to_string())));
    }
}
