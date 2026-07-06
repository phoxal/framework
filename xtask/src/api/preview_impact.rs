use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;
use serde::Serialize;

use crate::api::manifest::{self, ApiContractManifestGeneration, ContractDiff, ContractKey};
use crate::api::sync_features::GenerationChannel;
use crate::catalog::generate::{default_catalog_path, workspace_relative_path};
use crate::catalog::schema::{ArtifactStatus, CatalogEntry, CatalogRevision};
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
    pub target_triples: Vec<String>,
    pub status: BTreeMap<String, ArtifactStatus>,
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
    catalog: Option<&CatalogRevision>,
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
    catalog: &CatalogRevision,
    generation: &str,
) -> Result<GenerationImpact> {
    let base_generation = manifest::base_generation_name(api_manifest, generation)?;
    let changed_contracts = manifest::diff_contracts(api_manifest, &base_generation, generation)?;
    let changed_keys = changed_contracts
        .iter()
        .map(ContractDiff::key)
        .collect::<BTreeSet<_>>();
    let affected_artifacts = affected_artifacts(catalog, generation, &changed_keys);
    let ready_to_promote = affected_artifacts.iter().all(|artifact| artifact.complete);

    Ok(GenerationImpact {
        generation: generation.to_string(),
        base_generation,
        changed_contracts,
        affected_artifacts,
        ready_to_promote,
    })
}

fn affected_artifacts(
    catalog: &CatalogRevision,
    target_generation: &str,
    changed_keys: &BTreeSet<ContractKey>,
) -> Vec<AffectedArtifact> {
    if changed_keys.is_empty() {
        return Vec::new();
    }

    let mut affected = BTreeMap::<String, &CatalogEntry>::new();
    for entry in &catalog.entries {
        if entry.contract_uses.iter().any(|contract| {
            changed_keys.contains(&ContractKey {
                family: contract.family.clone(),
                topic: contract.topic_template.clone(),
            })
        }) {
            insert_latest_entry(&mut affected, entry);
        }
    }

    affected
        .into_iter()
        .map(|(package, source_entry)| {
            let target_entry = latest_generation_entry(catalog, &package, target_generation);
            affected_artifact(package, source_entry, target_entry, target_generation)
        })
        .collect()
}

fn insert_latest_entry<'a>(
    entries: &mut BTreeMap<String, &'a CatalogEntry>,
    entry: &'a CatalogEntry,
) {
    match entries.get(&entry.package) {
        Some(existing) if existing.version >= entry.version => {}
        _ => {
            entries.insert(entry.package.clone(), entry);
        }
    }
}

fn latest_generation_entry<'a>(
    catalog: &'a CatalogRevision,
    package: &str,
    generation: &str,
) -> Option<&'a CatalogEntry> {
    catalog
        .entries
        .iter()
        .filter(|entry| entry.package == package && entry.api_generation == generation)
        .max_by(|left, right| left.version.cmp(&right.version))
}

fn affected_artifact(
    package: String,
    source_entry: &CatalogEntry,
    target_entry: Option<&CatalogEntry>,
    expected_generation: &str,
) -> AffectedArtifact {
    let entry = target_entry.unwrap_or(source_entry);
    let (complete, reason) = match target_entry {
        Some(entry) => completeness(entry),
        None => (
            false,
            Some(format!("missing {expected_generation} catalog entry")),
        ),
    };

    AffectedArtifact {
        package,
        kind: entry.kind,
        target_version: target_entry.map(|entry| entry.version.clone()),
        target_generation: target_entry.map(|entry| entry.api_generation.clone()),
        target_triples: target_entry
            .map(|entry| entry.target_triples.clone())
            .unwrap_or_default(),
        status: target_entry
            .map(|entry| entry.status.clone())
            .unwrap_or_default(),
        complete,
        reason,
    }
}

fn completeness(entry: &CatalogEntry) -> (bool, Option<String>) {
    if entry.target_triples.is_empty() {
        return (
            false,
            Some("target entry has no target triples".to_string()),
        );
    }

    let incomplete = entry
        .target_triples
        .iter()
        .filter_map(|triple| match entry.status.get(triple) {
            Some(ArtifactStatus::Released) => None,
            Some(status) => Some(format!("{triple} is {status:?}")),
            None => Some(format!("{triple} has no catalog status")),
        })
        .collect::<Vec<_>>();

    if incomplete.is_empty() {
        (true, None)
    } else {
        (false, Some(incomplete.join(", ")))
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
            for triple in &artifact.target_triples {
                let status = artifact
                    .status
                    .get(triple)
                    .map(|status| format!("{status:?}"))
                    .unwrap_or_else(|| "missing".to_string());
                out.push_str(&format!("  - {triple}: {status}\n"));
            }
            if artifact.target_triples.is_empty()
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
    use crate::catalog::schema::{
        CatalogIntegrity, CatalogProvenance, Channel, ContractUse, EngineVersions, LaunchFacts,
        RouterFacts, SystemdFacts,
    };

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
        status: ArtifactStatus,
    ) -> CatalogEntry {
        let mut channels = BTreeMap::new();
        channels.insert(Channel::Preview, "0.2.0".to_string());
        let mut statuses = BTreeMap::new();
        statuses.insert("x86_64-unknown-linux-gnu".to_string(), status);
        CatalogEntry {
            package: package.to_string(),
            kind: ArtifactKind::Service,
            version: "0.2.0".to_string(),
            api_generation: generation.to_string(),
            contract_uses: vec![ContractUse {
                family: "battery::State".to_string(),
                topic_template: "battery/state".to_string(),
                direction: "publish".to_string(),
                schema_id: schema_id.to_string(),
            }],
            target_triples: vec!["x86_64-unknown-linux-gnu".to_string()],
            release_assets: BTreeMap::new(),
            launch_facts: LaunchFacts {
                participant_kind: "service".to_string(),
                participant_class: "checked".to_string(),
                router: RouterFacts {
                    needs_zenoh_router: true,
                },
                systemd: SystemdFacts {
                    groups: Vec::new(),
                    devices: Vec::new(),
                    source: "fixture".to_string(),
                },
            },
            engine_versions: EngineVersions {
                phoxal: Some("0.21.0".to_string()),
                phoxal_bus: None,
                zenoh: None,
            },
            channels,
            status: statuses,
            changed_contracts: Vec::new(),
        }
    }

    fn catalog(entries: Vec<CatalogEntry>) -> CatalogRevision {
        CatalogRevision {
            schema: "phoxal.artifact-catalog/v0".to_string(),
            format_version: 1,
            revision: "fixture".to_string(),
            integrity: CatalogIntegrity {
                algorithm: "sha256".to_string(),
                canonicalization: "fixture".to_string(),
                sha256: "fixture".to_string(),
            },
            provenance: CatalogProvenance {
                generator: "fixture".to_string(),
                official_set: "fixture".to_string(),
                emit_apis: "fixture".to_string(),
                release_assets: "fixture".to_string(),
                plan_01: "fixture".to_string(),
            },
            signature: None,
            entries,
        }
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
                ArtifactStatus::Released,
            ),
            catalog_entry(
                "phoxal/service-battery",
                "y2026_2",
                "2222222222222222",
                ArtifactStatus::Pending,
            ),
        ]);

        let report = build_report(&manifest, Some(&catalog))?;
        let preview = &report.previews[0];
        assert_eq!(preview.changed_contracts.len(), 1);
        assert_eq!(preview.affected_artifacts.len(), 1);
        assert!(!preview.ready_to_promote);
        assert_eq!(
            preview.affected_artifacts[0].reason.as_deref(),
            Some("x86_64-unknown-linux-gnu is Pending")
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
