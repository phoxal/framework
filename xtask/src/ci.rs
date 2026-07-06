use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use anyhow::{Context, Result, bail};
use clap::{Args as ClapArgs, Subcommand};
use serde::Serialize;
use serde_json::Value;

use crate::catalog::check::{compare_catalogs, validate_coverage};
use crate::catalog::generate::{
    GenerateOptions, InputMode, build_catalog_revision, workspace_relative_path,
};
use crate::catalog::schema::{ArtifactStatus, CatalogRevision};
use crate::catalog::verify::verify_catalog_path;
use crate::release::package;
use crate::release::plan::{ReleasePlan, load_release_plan};
use crate::release::upload::github_release_asset_names;
use crate::workspace::Workspace;

#[derive(Debug, Subcommand)]
pub enum Command {
    ReleaseGate(ReleaseGateArgs),
    CompatReport(CompatReportArgs),
}

#[derive(Debug, ClapArgs)]
pub struct ReleaseGateArgs {
    #[arg(
        long,
        value_name = "PATH",
        default_value = "target/xtask/catalog/phoxal-artifact-catalog.json"
    )]
    pub catalog: PathBuf,
    #[arg(long, value_name = "DIR", default_value = "target/xtask/release")]
    pub package_dir: PathBuf,
    #[arg(long, value_name = "PATH")]
    pub release_plan: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub previous_catalog: Option<PathBuf>,
    #[arg(long, value_name = "OWNER/REPO", env = "GITHUB_REPOSITORY")]
    pub repo: Option<String>,
    #[arg(long)]
    pub verify_github: bool,
    /// Local fixture escape hatch. CI must leave this unset.
    #[arg(long)]
    pub skip_topology: bool,
}

#[derive(Debug, ClapArgs)]
pub struct CompatReportArgs {
    #[arg(long, value_name = "PATH")]
    pub catalog: PathBuf,
    #[arg(long, value_name = "PATH")]
    pub audit_json: Option<PathBuf>,
    #[arg(
        long,
        value_name = "PATH",
        default_value = "target/xtask/compat-tracking.json"
    )]
    pub json_out: PathBuf,
    #[arg(
        long,
        value_name = "PATH",
        default_value = "target/xtask/compat-tracking.md"
    )]
    pub markdown_out: PathBuf,
    #[arg(long, value_name = "OWNER/REPO", env = "GITHUB_REPOSITORY")]
    pub repo: Option<String>,
    #[arg(long)]
    pub upsert_issue: bool,
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::ReleaseGate(args) => release_gate(args),
        Command::CompatReport(args) => compat_report(args),
    }
}

fn release_gate(args: ReleaseGateArgs) -> Result<()> {
    let workspace = Workspace::discover()?;
    let catalog_path = workspace_relative_path(&workspace, &args.catalog);
    let package_dir = package::workspace_relative_out_dir(&workspace, &args.package_dir);
    let release_plan = args
        .release_plan
        .as_deref()
        .map(|path| load_release_plan(&workspace_relative_path(&workspace, path)))
        .transpose()?;
    let previous_catalog = args
        .previous_catalog
        .as_deref()
        .map(|path| verify_catalog_path(&workspace_relative_path(&workspace, path)))
        .transpose()?;
    let catalog = verify_catalog_path(&catalog_path)?;
    validate_coverage(&catalog, workspace.official_artifacts())?;

    let host_triple = package::host_triple(workspace.root())?;
    let target_triples =
        release_plan_targets(&release_plan).unwrap_or_else(|| vec![host_triple.clone()]);
    let expected = build_catalog_revision(
        &workspace,
        &GenerateOptions {
            package_dir: package_dir.clone(),
            mode: InputMode::PackageOutputs,
            target_triples,
            release_plan: release_plan.clone(),
            previous_catalog,
        },
        &host_triple,
    )?;
    compare_catalogs(&catalog, &expected)?;

    if let Some(plan) = &release_plan {
        verify_release_plan_assets(
            &workspace,
            &catalog,
            plan,
            &package_dir,
            args.verify_github,
            args.repo.as_deref(),
        )?;
    }

    if !args.skip_topology {
        run_official_topology(workspace.root())?;
    }

    println!(
        "release gate passed: catalog {} covers {} entries",
        catalog.revision,
        catalog.entries.len()
    );
    Ok(())
}

fn release_plan_targets(plan: &Option<ReleasePlan>) -> Option<Vec<String>> {
    let plan = plan.as_ref()?;
    let mut targets = plan
        .matrix
        .include
        .iter()
        .map(|entry| entry.target.clone())
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    Some(targets)
}

fn verify_release_plan_assets(
    workspace: &Workspace,
    catalog: &CatalogRevision,
    plan: &ReleasePlan,
    package_dir: &std::path::Path,
    verify_github: bool,
    repo: Option<&str>,
) -> Result<()> {
    let catalog_entries = catalog
        .entries
        .iter()
        .map(|entry| (entry.package.as_str(), entry))
        .collect::<BTreeMap<_, _>>();

    for matrix in &plan.matrix.include {
        let artifact = workspace.official_artifact(&matrix.package)?;
        let output = package::read_packaged_output(artifact, package_dir, &matrix.target)?;
        let entry = catalog_entries
            .get(matrix.package.as_str())
            .with_context(|| format!("catalog missing {}", matrix.package))?;
        if entry.version != matrix.version {
            bail!(
                "{} catalog version {} did not match release-plan version {}",
                matrix.package,
                entry.version,
                matrix.version
            );
        }
        let asset = entry.release_assets.get(&matrix.target).with_context(|| {
            format!(
                "{} catalog entry is missing release asset for {}",
                matrix.package, matrix.target
            )
        })?;
        if asset.asset != output.tarball_name
            || asset.sha256 != output.tarball_sha256
            || asset.metadata.emit_apis != output.metadata_name
            || asset.metadata.emit_apis_sha256 != output.metadata_sha256
            || asset.metadata.sha256_file != output.checksum_name
        {
            bail!(
                "{} {} catalog asset facts do not match packaged outputs",
                matrix.package,
                matrix.target
            );
        }
        if verify_github {
            let repo = repo.context("--verify-github requires --repo or GITHUB_REPOSITORY")?;
            let existing = github_release_asset_names(repo, &matrix.tag)?;
            for name in [
                &output.tarball_name,
                &output.checksum_name,
                &output.metadata_name,
            ] {
                if !existing.contains(name) {
                    bail!(
                        "GitHub release {} is missing uploaded asset {}",
                        matrix.tag,
                        name
                    );
                }
            }
        }
    }
    Ok(())
}

fn run_official_topology(root: &std::path::Path) -> Result<()> {
    let output = ProcessCommand::new("cargo")
        .args([
            "test",
            "-p",
            "phoxal",
            "--test",
            "official_set_topology",
            "--",
            "--nocapture",
        ])
        .current_dir(root)
        .output()
        .context("failed to spawn official_set_topology test")?;
    if !output.status.success() {
        bail!(
            "official_set_topology failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn compat_report(args: CompatReportArgs) -> Result<()> {
    let workspace = Workspace::discover()?;
    let catalog_path = workspace_relative_path(&workspace, &args.catalog);
    let catalog = verify_catalog_path(&catalog_path)?;
    let audit = args
        .audit_json
        .as_deref()
        .map(|path| read_audit_findings(&workspace_relative_path(&workspace, path)))
        .transpose()?
        .unwrap_or_default();
    let report = build_compat_report(&catalog, audit);
    write_report_json(
        &workspace_relative_path(&workspace, &args.json_out),
        &report,
    )?;
    write_report_markdown(
        &workspace_relative_path(&workspace, &args.markdown_out),
        &report,
    )?;
    if args.upsert_issue {
        let repo = args
            .repo
            .as_deref()
            .context("--upsert-issue requires --repo or GITHUB_REPOSITORY")?;
        upsert_tracking_issue(
            repo,
            &workspace_relative_path(&workspace, &args.markdown_out),
            &report,
        )?;
    }
    println!(
        "compat report: {} open cause(s), {} affected artifact target(s)",
        report.causes.len(),
        report.affected_targets.len()
    );
    Ok(())
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
struct CompatReport {
    causes: Vec<CompatCause>,
    affected_targets: Vec<AffectedTarget>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CompatCause {
    kind: String,
    summary: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct AffectedTarget {
    package: String,
    version: String,
    target: String,
    reason: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct AuditFinding {
    advisory: String,
    package: Option<String>,
}

fn build_compat_report(catalog: &CatalogRevision, audit: Vec<AuditFinding>) -> CompatReport {
    let mut report = CompatReport::default();
    for finding in audit {
        report.causes.push(CompatCause {
            kind: "rustsec".to_string(),
            summary: match finding.package {
                Some(package) => format!("{} affects package {}", finding.advisory, package),
                None => finding.advisory,
            },
        });
    }

    let mut affected = BTreeSet::new();
    for entry in &catalog.entries {
        for (target, status) in &entry.status {
            if *status != ArtifactStatus::Released {
                affected.insert(AffectedTarget {
                    package: entry.package.clone(),
                    version: entry.version.clone(),
                    target: target.clone(),
                    reason: format!("catalog status is {:?}", status).to_lowercase(),
                });
            }
        }
    }

    if !report.causes.is_empty() {
        for entry in &catalog.entries {
            for target in &entry.target_triples {
                affected.insert(AffectedTarget {
                    package: entry.package.clone(),
                    version: entry.version.clone(),
                    target: target.clone(),
                    reason:
                        "cargo-audit reported an advisory; cataloged auditable binaries need review"
                            .to_string(),
                });
            }
        }
    }

    report.affected_targets = affected.into_iter().collect();
    report
}

fn read_audit_findings(path: &std::path::Path) -> Result<Vec<AuditFinding>> {
    let text =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    let mut findings = BTreeMap::<String, AuditFinding>::new();
    collect_audit_findings(&value, &mut findings);
    Ok(findings.into_values().collect())
}

fn collect_audit_findings(value: &Value, findings: &mut BTreeMap<String, AuditFinding>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_audit_findings(value, findings);
            }
        }
        Value::Object(object) => {
            if let Some(advisory) = advisory_id_from_object(object) {
                let package = package_name_from_object(object).map(str::to_string);
                findings
                    .entry(advisory.to_string())
                    .or_insert(AuditFinding {
                        advisory: advisory.to_string(),
                        package,
                    });
            }
            for value in object.values() {
                collect_audit_findings(value, findings);
            }
        }
        _ => {}
    }
}

fn advisory_id_from_object(object: &serde_json::Map<String, Value>) -> Option<&str> {
    object
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| value.starts_with("RUSTSEC-"))
        .or_else(|| {
            object
                .get("advisory")
                .and_then(Value::as_object)
                .and_then(|advisory| advisory.get("id"))
                .and_then(Value::as_str)
                .filter(|value| value.starts_with("RUSTSEC-"))
        })
}

fn package_name_from_object(object: &serde_json::Map<String, Value>) -> Option<&str> {
    object
        .get("package")
        .and_then(Value::as_object)
        .and_then(|package| package.get("name"))
        .and_then(Value::as_str)
        .or_else(|| object.get("package").and_then(Value::as_str))
        .or_else(|| object.get("crate").and_then(Value::as_str))
}

fn write_report_json(path: &std::path::Path, report: &CompatReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut json =
        serde_json::to_string_pretty(report).context("failed to serialize compat report")?;
    json.push('\n');
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}

fn write_report_markdown(path: &std::path::Path, report: &CompatReport) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut file =
        fs::File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
    writeln!(file, "# Compatibility Tracking")?;
    writeln!(file)?;
    if report.causes.is_empty() && report.affected_targets.is_empty() {
        writeln!(file, "No catalog audit or staleness findings are open.")?;
        return Ok(());
    }
    if !report.causes.is_empty() {
        writeln!(file, "## Causes")?;
        for cause in &report.causes {
            writeln!(file, "- {}: {}", cause.kind, cause.summary)?;
        }
        writeln!(file)?;
    }
    if !report.affected_targets.is_empty() {
        writeln!(file, "## Affected Artifact Targets")?;
        for target in &report.affected_targets {
            writeln!(
                file,
                "- {} {} on {} - {}",
                target.package, target.version, target.target, target.reason
            )?;
        }
        writeln!(file)?;
        writeln!(
            file,
            "Remediation: open a human PR with `cargo xtask release bump --affected` or `cargo xtask release bump --all`, then let the normal release path publish new assets."
        )?;
    }
    Ok(())
}

fn upsert_tracking_issue(
    repo: &str,
    markdown_path: &std::path::Path,
    report: &CompatReport,
) -> Result<()> {
    ensure_tracking_label(repo)?;
    let issue = open_tracking_issue(repo)?;
    if report.causes.is_empty() && report.affected_targets.is_empty() {
        if let Some(number) = issue {
            gh(&[
                "issue",
                "close",
                &number.to_string(),
                "--repo",
                repo,
                "--comment",
                "Auto-closing: the compatibility tracking report is empty.",
            ])?;
        }
        return Ok(());
    }

    match issue {
        Some(number) => {
            gh(&[
                "issue",
                "edit",
                &number.to_string(),
                "--repo",
                repo,
                "--title",
                "Compatibility tracking",
                "--body-file",
                path_str(markdown_path)?,
            ])?;
        }
        None => {
            gh(&[
                "issue",
                "create",
                "--repo",
                repo,
                "--title",
                "Compatibility tracking",
                "--body-file",
                path_str(markdown_path)?,
                "--label",
                "compat-tracking",
            ])?;
        }
    }
    Ok(())
}

fn ensure_tracking_label(repo: &str) -> Result<()> {
    let status = ProcessCommand::new("gh")
        .args([
            "label",
            "create",
            "compat-tracking",
            "--repo",
            repo,
            "--color",
            "d4c5f9",
            "--description",
            "Automated catalog compatibility tracking",
            "--force",
        ])
        .status()
        .context("failed to spawn gh label create")?;
    if !status.success() {
        bail!("gh label create compat-tracking failed with status {status}");
    }
    Ok(())
}

fn open_tracking_issue(repo: &str) -> Result<Option<u64>> {
    let output = ProcessCommand::new("gh")
        .args([
            "issue",
            "list",
            "--repo",
            repo,
            "--label",
            "compat-tracking",
            "--state",
            "open",
            "--json",
            "number",
            "--jq",
            ".[0].number",
        ])
        .output()
        .context("failed to spawn gh issue list")?;
    if !output.status.success() {
        bail!(
            "gh issue list failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let text = String::from_utf8(output.stdout).context("gh issue list output was not UTF-8")?;
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Ok(None);
    }
    trimmed
        .parse::<u64>()
        .map(Some)
        .with_context(|| format!("gh issue list returned non-numeric issue number '{trimmed}'"))
}

fn gh(args: &[&str]) -> Result<()> {
    let status = ProcessCommand::new("gh")
        .args(args)
        .status()
        .context("failed to spawn gh")?;
    if !status.success() {
        bail!(
            "gh command failed with status {status}: gh {}",
            args.join(" ")
        );
    }
    Ok(())
}

fn path_str(path: &std::path::Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("{} is not valid UTF-8", path.display()))
}

impl Ord for AffectedTarget {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (&self.package, &self.target, &self.reason).cmp(&(
            &other.package,
            &other.target,
            &other.reason,
        ))
    }
}

impl PartialOrd for AffectedTarget {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::generate::tests_support::fixture_catalog;

    #[test]
    fn compat_report_flags_non_released_targets() -> Result<()> {
        let mut catalog = fixture_catalog()?;
        catalog.entries[0].status.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            ArtifactStatus::Pending,
        );
        catalog = catalog.finalize()?;

        let report = build_compat_report(&catalog, Vec::new());

        assert_eq!(report.affected_targets.len(), 1);
        assert!(report.affected_targets[0].reason.contains("pending"));
        Ok(())
    }

    #[test]
    fn audit_json_finding_opens_cause() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let audit_path = temp.path().join("audit.json");
        fs::write(
            &audit_path,
            r#"{ "vulnerabilities": { "list": [
  { "advisory": { "id": "RUSTSEC-2026-0001" }, "package": { "name": "zenoh" } }
] } }"#,
        )?;

        let findings = read_audit_findings(&audit_path)?;

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].advisory, "RUSTSEC-2026-0001");
        assert_eq!(findings[0].package.as_deref(), Some("zenoh"));
        Ok(())
    }
}
