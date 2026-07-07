use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use phoxal::catalog::{ArtifactEntry, Manifest};
use serde::Serialize;

use crate::api::manifest::{self, ApiContractManifestGeneration, ContractDiff};
use crate::api::sync_features::GenerationChannel;
use crate::catalog::generate::{default_catalog_path, workspace_relative_path};
use crate::catalog::verify::verify_catalog_path;
use crate::workspace::{ArtifactKind, Workspace};

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long)]
    pub json: bool,
    #[arg(long, value_name = "PATH")]
    pub catalog: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub json_out: Option<PathBuf>,
    /// Append the human report to the GitHub step summary named by
    /// GITHUB_STEP_SUMMARY.
    #[arg(long)]
    pub github_summary: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct PreviewImpactReport {
    pub previews: Vec<GenerationImpact>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct GenerationImpact {
    pub generation: String,
    pub base_generation: String,
    pub changed_contracts: Vec<ContractDiff>,
    pub affected_artifacts: Vec<AffectedArtifact>,
    pub ready_to_promote: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct AffectedArtifact {
    pub package: String,
    pub kind: ArtifactKind,
    pub target_version: Option<String>,
    pub target_generation: Option<String>,
    /// Triples the target-generation catalog entry has an actual built
    /// artifact for (its `artifacts` map keys) - there is no separate
    /// "planned but not yet built" status anymore.
    pub built_triples: Vec<String>,
    pub complete: bool,
    pub reason: Option<String>,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let api_manifest = manifest::load_from_workspace(&workspace)?;
    let has_previews = api_manifest
        .iter()
        .any(|generation| generation.channel == GenerationChannel::Preview);

    let catalog_path = args
        .catalog
        .as_deref()
        .map(|path| workspace_relative_path(&workspace, path))
        .unwrap_or_else(|| default_catalog_path(&workspace));
    let catalog = if has_previews {
        Some(verify_catalog_path(&catalog_path)?)
    } else {
        None
    };

    let report = build_report(&api_manifest, catalog.as_ref())?;
    emit_report(&workspace, &args, &report)?;
    Ok(())
}

pub(crate) fn build_report(
    api_manifest: &[ApiContractManifestGeneration],
    catalog: Option<&Manifest>,
) -> Result<PreviewImpactReport> {
    let mut previews = Vec::new();
    for generation in api_manifest
        .iter()
        .filter(|generation| generation.channel == GenerationChannel::Preview)
    {
        let catalog = catalog.with_context(|| {
            format!(
                "catalog is required to compute preview impact for {}",
                generation.name
            )
        })?;
        previews.push(build_generation_impact(
            api_manifest,
            catalog,
            &generation.name,
        )?);
    }
    Ok(PreviewImpactReport { previews })
}

pub(crate) fn build_generation_impact(
    api_manifest: &[ApiContractManifestGeneration],
    catalog: &Manifest,
    generation: &str,
) -> Result<GenerationImpact> {
    let base_generation = manifest::base_generation_name(api_manifest, generation)?;
    let changed_contracts = manifest::diff_contracts(api_manifest, &base_generation, generation)?;
    // The catalog's `Contract` carries only `family`/`schema_id` (no
    // `topic_template`); a contract family is already the practical unique key
    // within one API generation (the cross-artifact schema-agreement rule
    // keys on family alone), so matching drops the topic component too.
    let changed_families = changed_contracts
        .iter()
        .map(|diff| diff.family.clone())
        .collect::<BTreeSet<_>>();
    let affected_artifacts = affected_artifacts(catalog, generation, &changed_families);
    let ready_to_promote = affected_artifacts.iter().all(|artifact| artifact.complete);

    Ok(GenerationImpact {
        generation: generation.to_string(),
        base_generation,
        changed_contracts,
        affected_artifacts,
        ready_to_promote,
    })
}

/// Every runtime entry across the catalog's four kind arrays, tagged with the
/// xtask-side [`ArtifactKind`] it belongs to (the manifest itself no longer
/// carries a per-entry `kind` - the array an entry lives in *is* its kind).
fn runtime_entries(catalog: &Manifest) -> Vec<(ArtifactKind, &ArtifactEntry)> {
    catalog
        .services
        .iter()
        .map(|entry| (ArtifactKind::Service, entry))
        .chain(
            catalog
                .drivers
                .iter()
                .map(|entry| (ArtifactKind::ComponentDriver, entry)),
        )
        .chain(
            catalog
                .tools
                .iter()
                .map(|entry| (ArtifactKind::Tool, entry)),
        )
        .chain(
            catalog
                .simulators
                .iter()
                .map(|entry| (ArtifactKind::Simulator, entry)),
        )
        .collect()
}

fn affected_artifacts(
    catalog: &Manifest,
    target_generation: &str,
    changed_families: &BTreeSet<String>,
) -> Vec<AffectedArtifact> {
    if changed_families.is_empty() {
        return Vec::new();
    }

    let entries = runtime_entries(catalog);
    // Only the package identity and its (generation-invariant) kind matter
    // here; `latest_generation_entry` below does the real per-generation
    // version selection for the target generation.
    let mut affected = BTreeMap::<String, ArtifactKind>::new();
    for (kind, entry) in &entries {
        if entry
            .contracts
            .iter()
            .any(|contract| changed_families.contains(&contract.family))
        {
            affected.entry(entry.package.clone()).or_insert(*kind);
        }
    }

    affected
        .into_iter()
        .map(|(package, kind)| {
            let target_entry = latest_generation_entry(&entries, &package, target_generation);
            affected_artifact(package, kind, target_entry, target_generation)
        })
        .collect()
}

fn latest_generation_entry<'a>(
    entries: &'a [(ArtifactKind, &'a ArtifactEntry)],
    package: &str,
    generation: &str,
) -> Option<&'a ArtifactEntry> {
    entries
        .iter()
        .filter(|(_, entry)| entry.package == package && entry.api_generation == generation)
        .map(|(_, entry)| *entry)
        .max_by(|left, right| left.version.cmp(&right.version))
}

fn affected_artifact(
    package: String,
    kind: ArtifactKind,
    target_entry: Option<&ArtifactEntry>,
    expected_generation: &str,
) -> AffectedArtifact {
    let (complete, reason) = match target_entry {
        Some(entry) => completeness(entry),
        None => (
            false,
            Some(format!("missing {expected_generation} catalog entry")),
        ),
    };

    AffectedArtifact {
        package,
        kind,
        target_version: target_entry.map(|entry| entry.version.clone()),
        target_generation: target_entry.map(|entry| entry.api_generation.clone()),
        built_triples: target_entry
            .map(|entry| entry.artifacts.keys().cloned().collect())
            .unwrap_or_default(),
        complete,
        reason,
    }
}

/// An entry is "complete" (ready to promote) once it has at least one real
/// built artifact - there is no separate "planned but not yet built" status
/// anymore, so an empty `artifacts` map is the only "not released" signal.
fn completeness(entry: &ArtifactEntry) -> (bool, Option<String>) {
    if entry.artifacts.is_empty() {
        (
            false,
            Some("target entry has no released artifacts for any target".to_string()),
        )
    } else {
        (true, None)
    }
}

fn emit_report(workspace: &Workspace, args: &Args, report: &PreviewImpactReport) -> Result<()> {
    if let Some(path) = &args.json_out {
        let path = workspace_relative_path(workspace, path);
        write_json(&path, report)?;
    }

    if args.github_summary {
        write_github_summary(report)?;
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        print!("{}", human_report(report));
    }

    Ok(())
}

fn write_json(path: &Path, report: &PreviewImpactReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut json = serde_json::to_string_pretty(report)?;
    json.push('\n');
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}

fn write_github_summary(report: &PreviewImpactReport) -> Result<()> {
    let summary = std::env::var_os("GITHUB_STEP_SUMMARY")
        .context("--github-summary requires GITHUB_STEP_SUMMARY to be set")?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&summary)
        .with_context(|| {
            format!(
                "failed to open GitHub step summary {}",
                PathBuf::from(&summary).display()
            )
        })?;
    file.write_all(human_report(report).as_bytes())
        .context("failed to write GitHub step summary")
}

pub(crate) fn human_report(report: &PreviewImpactReport) -> String {
    let mut out = String::new();
    out.push_str("# API Preview Impact\n\n");
    if report.previews.is_empty() {
        out.push_str("No preview API generations.\n");
        return out;
    }

    for preview in &report.previews {
        out.push_str(&format!(
            "## {} extends {}\n\n",
            preview.generation, preview.base_generation
        ));
        out.push_str(&format!(
            "Changed contracts: {}\n\n",
            preview.changed_contracts.len()
        ));
        for contract in &preview.changed_contracts {
            let from = contract.from_schema_id.as_deref().unwrap_or("-");
            let to = contract.to_schema_id.as_deref().unwrap_or("-");
            out.push_str(&format!(
                "- {:?}: {} on {} ({} -> {})\n",
                contract.kind, contract.family, contract.topic, from, to
            ));
        }
        if preview.changed_contracts.is_empty() {
            out.push_str("- none\n");
        }
        out.push('\n');
        out.push_str(&format!(
            "Affected artifacts: {}\n\n",
            preview.affected_artifacts.len()
        ));
        for artifact in &preview.affected_artifacts {
            let status = if artifact.complete {
                "complete"
            } else {
                "incomplete"
            };
            let reason_suffix = artifact
                .reason
                .as_deref()
                .map(|reason| format!(" - {reason}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "- {} {}{}\n",
                artifact.package, status, reason_suffix
            ));
            for triple in &artifact.built_triples {
                out.push_str(&format!("  - {triple}: released\n"));
            }
            if artifact.built_triples.is_empty()
                && let Some(reason) = &artifact.reason
            {
                out.push_str(&format!("  - {reason}\n"));
            }
        }
        if preview.affected_artifacts.is_empty() {
            out.push_str("- none\n");
        }
        out.push('\n');
        if preview.ready_to_promote {
            out.push_str("Ready to promote: yes\n\n");
        } else {
            out.push_str("Ready to promote: no\n\n");
        }
    }

    out
}

pub(crate) fn ensure_ready_to_promote(impact: &GenerationImpact) -> Result<()> {
    if impact.ready_to_promote {
        return Ok(());
    }

    let incomplete = impact
        .affected_artifacts
        .iter()
        .filter(|artifact| !artifact.complete)
        .map(|artifact| {
            let reason = artifact.reason.as_deref().unwrap_or("incomplete");
            format!("{} ({reason})", artifact.package)
        })
        .collect::<Vec<_>>();

    bail!(
        "promotion of {} is blocked; affected artifacts are incomplete: {}",
        impact.generation,
        incomplete.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::catalog::{Artifact, Channel, Contract};

    fn generation(
        name: &str,
        channel: GenerationChannel,
        extends: Option<&str>,
        contracts: Vec<manifest::ContractShape>,
    ) -> ApiContractManifestGeneration {
        ApiContractManifestGeneration {
            name: name.to_string(),
            channel,
            extends: extends.map(str::to_string),
            contracts,
        }
    }

    fn contract(family: &str, topic: &str, schema_id: &str) -> manifest::ContractShape {
        manifest::ContractShape {
            family: family.to_string(),
            topic: topic.to_string(),
            schema_id: schema_id.to_string(),
        }
    }

    fn catalog_entry(
        package: &str,
        generation: &str,
        schema_id: &str,
        released: bool,
    ) -> ArtifactEntry {
        let mut channels = BTreeMap::new();
        channels.insert(Channel::Preview, "0.2.0".to_string());
        let mut artifacts = BTreeMap::new();
        if released {
            artifacts.insert(
                "x86_64-unknown-linux-gnu".to_string(),
                Artifact {
                    tarball: "fixture.tar.zst".to_string(),
                    sha256: "0".repeat(64),
                },
            );
        }
        ArtifactEntry {
            package: package.to_string(),
            version: "0.2.0".to_string(),
            api_generation: generation.to_string(),
            contracts: vec![Contract {
                family: "battery::State".to_string(),
                schema_id: schema_id.to_string(),
            }],
            config_schema: Some(serde_json::json!({ "type": "object" })),
            bus_abi: "phoxal-bus/v0".to_string(),
            artifacts,
            channels,
            changed_contracts: Vec::new(),
        }
    }

    fn catalog(services: Vec<ArtifactEntry>) -> Manifest {
        Manifest::new(Vec::new(), services, Vec::new(), Vec::new(), Vec::new())
            .finalize()
            .expect("fixture manifest finalizes")
    }

    #[test]
    fn impact_marks_changed_contract_artifact_incomplete_until_released() -> Result<()> {
        let manifest = vec![
            generation(
                "y2026_1",
                GenerationChannel::Stable,
                None,
                vec![contract(
                    "battery::State",
                    "battery/state",
                    "1111111111111111",
                )],
            ),
            generation(
                "y2026_2",
                GenerationChannel::Preview,
                Some("y2026_1"),
                vec![contract(
                    "battery::State",
                    "battery/state",
                    "2222222222222222",
                )],
            ),
        ];
        let catalog = catalog(vec![
            catalog_entry(
                "phoxal/service-battery",
                "y2026_1",
                "1111111111111111",
                true,
            ),
            catalog_entry(
                "phoxal/service-battery",
                "y2026_2",
                "2222222222222222",
                false,
            ),
        ]);

        let report = build_report(&manifest, Some(&catalog))?;
        let preview = &report.previews[0];
        assert_eq!(preview.changed_contracts.len(), 1);
        assert_eq!(preview.affected_artifacts.len(), 1);
        assert!(!preview.ready_to_promote);
        assert_eq!(
            preview.affected_artifacts[0].reason.as_deref(),
            Some("target entry has no released artifacts for any target")
        );
        let err = ensure_ready_to_promote(preview).unwrap_err();
        assert!(
            err.to_string().contains("promotion of y2026_2 is blocked"),
            "{err}"
        );

        Ok(())
    }

    #[test]
    fn empty_affected_set_is_ready_to_promote() -> Result<()> {
        let manifest = vec![
            generation(
                "y2026_1",
                GenerationChannel::Stable,
                None,
                vec![contract(
                    "battery::State",
                    "battery/state",
                    "1111111111111111",
                )],
            ),
            generation(
                "y2026_2",
                GenerationChannel::Preview,
                Some("y2026_1"),
                vec![contract(
                    "battery::State",
                    "battery/state",
                    "1111111111111111",
                )],
            ),
        ];
        let report = build_report(&manifest, Some(&catalog(Vec::new())))?;

        assert!(report.previews[0].changed_contracts.is_empty());
        assert!(report.previews[0].affected_artifacts.is_empty());
        assert!(report.previews[0].ready_to_promote);

        Ok(())
    }
}
