use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use serde::{Deserialize, Serialize};

use crate::api::manifest::{self, ApiContractManifestGeneration, ContractShape};
use crate::api::sync_features::GenerationChannel;
use crate::workspace::Workspace;

const BASELINE_PATH: &str = "phoxal-api/promoted-shapes.json";
const BASELINE_SCHEMA: &str = "phoxal.promoted-api-shapes/v0";

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Fail if the committed promoted-shapes baseline does not match the current
    /// stable generations.
    #[arg(long, conflicts_with = "write")]
    pub check: bool,
    /// Rewrite the committed promoted-shapes baseline.
    #[arg(long, conflicts_with = "check")]
    pub write: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct FrozenShapes {
    schema: String,
    generations: Vec<FrozenGeneration>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct FrozenGeneration {
    name: String,
    contracts: Vec<ContractShape>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Mode {
    Check,
    Write,
}

impl Args {
    fn mode(&self) -> Mode {
        if self.write { Mode::Write } else { Mode::Check }
    }
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let api_manifest = manifest::load_from_workspace(&workspace)?;
    let expected = frozen_shapes_from_manifest(&api_manifest);
    let baseline_path = workspace.root().join(BASELINE_PATH);

    match args.mode() {
        Mode::Check => {
            let actual = read_baseline(&baseline_path)?;
            compare_baseline(&actual, &expected)?;
            println!(
                "promoted API shapes match {} stable generation(s)",
                expected.generations.len()
            );
            Ok(())
        }
        Mode::Write => {
            write_baseline(&baseline_path, &expected)?;
            println!("rewrote {}", baseline_path.display());
            Ok(())
        }
    }
}

fn frozen_shapes_from_manifest(manifest: &[ApiContractManifestGeneration]) -> FrozenShapes {
    let generations = manifest
        .iter()
        .filter(|generation| generation.channel == GenerationChannel::Stable)
        .map(|generation| {
            let mut contracts = generation.contracts.clone();
            contracts.sort();
            FrozenGeneration {
                name: generation.name.clone(),
                contracts,
            }
        })
        .collect();
    FrozenShapes {
        schema: BASELINE_SCHEMA.to_string(),
        generations,
    }
}

fn read_baseline(path: &Path) -> Result<FrozenShapes> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let baseline: FrozenShapes = serde_json::from_str(&text)
        .with_context(|| format!("{} is not valid promoted-shapes JSON", path.display()))?;
    if baseline.schema != BASELINE_SCHEMA {
        bail!(
            "{} schema '{}' did not match expected '{}'",
            path.display(),
            baseline.schema,
            BASELINE_SCHEMA
        );
    }
    Ok(baseline)
}

fn write_baseline(path: &Path, baseline: &FrozenShapes) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut json = serde_json::to_string_pretty(baseline)?;
    json.push('\n');
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}

fn compare_baseline(actual: &FrozenShapes, expected: &FrozenShapes) -> Result<()> {
    if actual == expected {
        return Ok(());
    }

    let actual_names = actual
        .generations
        .iter()
        .map(|generation| generation.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let expected_names = expected
        .generations
        .iter()
        .map(|generation| generation.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "promoted API schema shapes changed; recorded [{actual_names}], current [{expected_names}]. \
         Run `cargo xtask api freeze-shapes --write` only in the promotion PR that legitimately \
         adds a promoted generation."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(family: &str, schema_id: &str) -> ContractShape {
        ContractShape {
            family: family.to_string(),
            topic: "fixture/topic".to_string(),
            schema_id: schema_id.to_string(),
        }
    }

    fn generation(
        name: &str,
        channel: GenerationChannel,
        contracts: Vec<ContractShape>,
    ) -> ApiContractManifestGeneration {
        ApiContractManifestGeneration {
            name: name.to_string(),
            channel,
            extends: None,
            contracts,
        }
    }

    #[test]
    fn promoted_shapes_include_stable_generations_only() {
        let frozen = frozen_shapes_from_manifest(&[
            generation(
                "y2026_1",
                GenerationChannel::Stable,
                vec![contract("drive::State", "1111111111111111")],
            ),
            generation(
                "y2026_2",
                GenerationChannel::Preview,
                vec![contract("drive::State", "2222222222222222")],
            ),
        ]);

        assert_eq!(frozen.generations.len(), 1);
        assert_eq!(frozen.generations[0].name, "y2026_1");
    }

    #[test]
    fn promoted_shape_change_fails_comparison() {
        let actual = FrozenShapes {
            schema: BASELINE_SCHEMA.to_string(),
            generations: vec![FrozenGeneration {
                name: "y2026_1".to_string(),
                contracts: vec![contract("drive::State", "1111111111111111")],
            }],
        };
        let expected = FrozenShapes {
            schema: BASELINE_SCHEMA.to_string(),
            generations: vec![FrozenGeneration {
                name: "y2026_1".to_string(),
                contracts: vec![contract("drive::State", "2222222222222222")],
            }],
        };

        let err = compare_baseline(&actual, &expected).unwrap_err();
        assert!(
            err.to_string()
                .contains("promoted API schema shapes changed")
        );
    }
}
