use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use crate::api::manifest::{self, ApiContractManifestGeneration};
use crate::api::preview_impact::{build_generation_impact, ensure_ready_to_promote};
use crate::api::sync_features::{self, ApiGeneration, GenerationChannel};
use crate::catalog::generate::{default_catalog_path, workspace_relative_path};
use crate::catalog::verify::verify_catalog_path;
use crate::workspace::Workspace;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Git ref or commit to compare against. In pull-request CI, pass the base
    /// SHA so removing a `preview` keyword is detected fail-closed.
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub catalog: Option<PathBuf>,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let current_manifest = manifest::load_from_workspace(&workspace)?;
    let base_ref = args.base.or_else(default_base_ref);
    let Some(base_ref) = base_ref else {
        println!("promotion gate: no base ref supplied; no promotion request detected");
        return Ok(());
    };

    let base_source = git_show(&workspace, &base_ref, "phoxal-api/src/lib.rs")?;
    let base_generations = sync_features::api_generations_from_source(&base_source)
        .with_context(|| format!("failed to scan phoxal-api/src/lib.rs at {base_ref}"))?;
    let promotions = removed_preview_keywords(&base_generations, &current_manifest);
    if promotions.is_empty() {
        println!("promotion gate: no preview keyword removals detected");
        return Ok(());
    }

    let catalog_path = args
        .catalog
        .as_deref()
        .map(|path| workspace_relative_path(&workspace, path))
        .unwrap_or_else(|| default_catalog_path(&workspace));
    let catalog = verify_catalog_path(&catalog_path)
        .with_context(|| format!("promotion gate requires catalog {}", catalog_path.display()))?;

    for generation in &promotions {
        let impact = build_generation_impact(&current_manifest, &catalog, generation)?;
        ensure_ready_to_promote(&impact)?;
        println!(
            "promotion gate: {} is complete for {} affected artifact(s)",
            generation,
            impact.affected_artifacts.len()
        );
    }

    Ok(())
}

fn default_base_ref() -> Option<String> {
    std::env::var("GITHUB_BASE_REF")
        .ok()
        .filter(|base| !base.trim().is_empty())
        .map(|base| format!("origin/{base}"))
}

fn git_show(workspace: &Workspace, git_ref: &str, path: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["show", &format!("{git_ref}:{path}")])
        .current_dir(workspace.root())
        .output()
        .with_context(|| format!("failed to spawn git show {git_ref}:{path}"))?;
    if !output.status.success() {
        bail!(
            "git show {git_ref}:{path} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).context("git show output was not UTF-8")
}

fn removed_preview_keywords(
    base: &[ApiGeneration],
    current: &[ApiContractManifestGeneration],
) -> Vec<String> {
    let current_channels = current
        .iter()
        .map(|generation| (generation.name.as_str(), generation.channel))
        .collect::<BTreeMap<_, _>>();
    base.iter()
        .filter(|generation| generation.channel == GenerationChannel::Preview)
        .filter(|generation| {
            current_channels
                .get(generation.name.as_str())
                .is_some_and(|channel| *channel == GenerationChannel::Stable)
        })
        .map(|generation| generation.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::manifest::ContractShape;

    fn base_generation(name: &str, channel: GenerationChannel) -> ApiGeneration {
        ApiGeneration {
            name: name.to_string(),
            channel,
        }
    }

    fn current_generation(name: &str, channel: GenerationChannel) -> ApiContractManifestGeneration {
        ApiContractManifestGeneration {
            name: name.to_string(),
            channel,
            extends: None,
            contracts: Vec::<ContractShape>::new(),
        }
    }

    #[test]
    fn detects_preview_keyword_removal_as_promotion_request() {
        let promotions = removed_preview_keywords(
            &[
                base_generation("y2026_1", GenerationChannel::Stable),
                base_generation("y2026_2", GenerationChannel::Preview),
            ],
            &[
                current_generation("y2026_1", GenerationChannel::Stable),
                current_generation("y2026_2", GenerationChannel::Stable),
            ],
        );

        assert_eq!(promotions, vec!["y2026_2"]);
    }

    #[test]
    fn stable_generation_and_still_preview_generation_are_not_promotions() {
        let promotions = removed_preview_keywords(
            &[
                base_generation("y2026_1", GenerationChannel::Stable),
                base_generation("y2026_2", GenerationChannel::Preview),
            ],
            &[
                current_generation("y2026_1", GenerationChannel::Stable),
                current_generation("y2026_2", GenerationChannel::Preview),
            ],
        );

        assert!(promotions.is_empty());
    }
}
