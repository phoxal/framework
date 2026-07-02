use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::api::sync_features::GenerationChannel;
use crate::workspace::Workspace;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApiContractManifestGeneration {
    pub name: String,
    pub channel: GenerationChannel,
    pub extends: Option<String>,
    pub contracts: Vec<ContractShape>,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContractShape {
    pub family: String,
    pub topic: String,
    pub schema_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContractDiffKind {
    Added,
    Removed,
    Changed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ContractDiff {
    pub kind: ContractDiffKind,
    pub family: String,
    pub topic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_schema_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_schema_id: Option<String>,
}

impl ContractDiff {
    pub(crate) fn key(&self) -> ContractKey {
        ContractKey {
            family: self.family.clone(),
            topic: self.topic.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ContractKey {
    pub family: String,
    pub topic: String,
}

pub(crate) fn load_from_workspace(
    workspace: &Workspace,
) -> Result<Vec<ApiContractManifestGeneration>> {
    load_from_root(workspace.root())
}

fn load_from_root(root: &Path) -> Result<Vec<ApiContractManifestGeneration>> {
    let output = Command::new("cargo")
        .args([
            "run",
            "--quiet",
            "-p",
            "phoxal-api",
            "--example",
            "contract_manifest",
        ])
        .current_dir(root)
        .output()
        .context("failed to spawn phoxal-api contract manifest example")?;

    if !output.status.success() {
        bail!(
            "phoxal-api contract manifest example failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    parse_manifest_json(&output.stdout)
}

fn parse_manifest_json(bytes: &[u8]) -> Result<Vec<ApiContractManifestGeneration>> {
    let mut manifest: Vec<ApiContractManifestGeneration> =
        serde_json::from_slice(bytes).context("contract manifest example did not emit JSON")?;
    for generation in &mut manifest {
        generation.contracts.sort();
        let mut seen = BTreeSet::new();
        for contract in &generation.contracts {
            if !seen.insert((&contract.family, &contract.topic)) {
                bail!(
                    "generation {} contains duplicate contract shape {} on {}",
                    generation.name,
                    contract.family,
                    contract.topic
                );
            }
        }
    }
    Ok(manifest)
}

pub(crate) fn generation<'a>(
    manifest: &'a [ApiContractManifestGeneration],
    name: &str,
) -> Result<&'a ApiContractManifestGeneration> {
    manifest
        .iter()
        .find(|generation| generation.name == name)
        .with_context(|| format!("unknown API generation {name}"))
}

pub(crate) fn base_generation_name(
    manifest: &[ApiContractManifestGeneration],
    name: &str,
) -> Result<String> {
    let index = manifest
        .iter()
        .position(|generation| generation.name == name)
        .with_context(|| format!("unknown API generation {name}"))?;
    if let Some(parent) = &manifest[index].extends {
        return Ok(parent.clone());
    }
    if index == 0 {
        bail!("generation {name} has no parent generation to diff against");
    }
    Ok(manifest[index - 1].name.clone())
}

pub(crate) fn diff_contracts(
    manifest: &[ApiContractManifestGeneration],
    from_generation: &str,
    to_generation: &str,
) -> Result<Vec<ContractDiff>> {
    let from = generation(manifest, from_generation)?;
    let to = generation(manifest, to_generation)?;
    diff_contract_lists(&from.contracts, &to.contracts)
}

pub(crate) fn diff_contract_lists(
    from: &[ContractShape],
    to: &[ContractShape],
) -> Result<Vec<ContractDiff>> {
    let from = contract_map(from)?;
    let to = contract_map(to)?;
    let keys = from
        .keys()
        .chain(to.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut diffs = Vec::new();

    for key in keys {
        match (from.get(&key), to.get(&key)) {
            (Some(from_schema), Some(to_schema)) if from_schema != to_schema => {
                diffs.push(ContractDiff {
                    kind: ContractDiffKind::Changed,
                    family: key.family,
                    topic: key.topic,
                    from_schema_id: Some(from_schema.clone()),
                    to_schema_id: Some(to_schema.clone()),
                });
            }
            (Some(from_schema), None) => diffs.push(ContractDiff {
                kind: ContractDiffKind::Removed,
                family: key.family,
                topic: key.topic,
                from_schema_id: Some(from_schema.clone()),
                to_schema_id: None,
            }),
            (None, Some(to_schema)) => diffs.push(ContractDiff {
                kind: ContractDiffKind::Added,
                family: key.family,
                topic: key.topic,
                from_schema_id: None,
                to_schema_id: Some(to_schema.clone()),
            }),
            _ => {}
        }
    }

    Ok(diffs)
}

fn contract_map(contracts: &[ContractShape]) -> Result<BTreeMap<ContractKey, String>> {
    let mut map = BTreeMap::new();
    for contract in contracts {
        let key = ContractKey {
            family: contract.family.clone(),
            topic: contract.topic.clone(),
        };
        if map.insert(key, contract.schema_id.clone()).is_some() {
            bail!(
                "duplicate contract shape {} on {}",
                contract.family,
                contract.topic
            );
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(family: &str, topic: &str, schema_id: &str) -> ContractShape {
        ContractShape {
            family: family.to_string(),
            topic: topic.to_string(),
            schema_id: schema_id.to_string(),
        }
    }

    #[test]
    fn diff_contract_lists_reports_changed_added_and_removed() -> Result<()> {
        let diffs = diff_contract_lists(
            &[
                contract("battery::State", "battery/state", "1111111111111111"),
                contract("drive::State", "drive/state", "2222222222222222"),
            ],
            &[
                contract("battery::State", "battery/state", "aaaaaaaaaaaaaaaa"),
                contract("safety::Status", "safety/state", "bbbbbbbbbbbbbbbb"),
            ],
        )?;

        assert_eq!(
            diffs,
            vec![
                ContractDiff {
                    kind: ContractDiffKind::Changed,
                    family: "battery::State".to_string(),
                    topic: "battery/state".to_string(),
                    from_schema_id: Some("1111111111111111".to_string()),
                    to_schema_id: Some("aaaaaaaaaaaaaaaa".to_string()),
                },
                ContractDiff {
                    kind: ContractDiffKind::Removed,
                    family: "drive::State".to_string(),
                    topic: "drive/state".to_string(),
                    from_schema_id: Some("2222222222222222".to_string()),
                    to_schema_id: None,
                },
                ContractDiff {
                    kind: ContractDiffKind::Added,
                    family: "safety::Status".to_string(),
                    topic: "safety/state".to_string(),
                    from_schema_id: None,
                    to_schema_id: Some("bbbbbbbbbbbbbbbb".to_string()),
                },
            ]
        );
        Ok(())
    }
}
