//! `cargo xtask ci ...` - checks that gate the release PR/wave, layered on
//! top of the `phoxal.catalog/v0` model (`phoxal::catalog`).
//!
//! NOTE (S1-catalog slice): this module used to also host `ci compat-report`
//! (a `schema_id`-era staleness/audit tracker) and a much larger
//! `release-gate` that re-derived and compared an "expected" catalog against
//! the recorded one (the old model's coherence check). Design doc
//! `organization/tmp/ci-release-refactor/design.md` §4.6 deletes that
//! coherence concept outright (no `schema_id`, no "two artifacts disagree on
//! a wire shape" failure mode is possible anymore), and §6/§7 mark
//! `compat-report` as a straight removal. `release_gate` below is trimmed to
//! exactly what design doc §4.4 still asks for pre-`--latest`: the
//! structural + coverage checks `catalog check` already performs, plus a
//! bounded "did this run's uploads actually land" guard. The richer
//! "`--verify-github` full re-verify of every past URL" is gone per §4.4 ("NOT
//! done: re-verifying every URL... immutable, never-deleted `build-*`
//! releases").
//!
//! This rewrite was **not** part of this slice's assigned scope (catalog
//! model + `catalog generate/check/verify`), but `ci.rs` had a hard compile
//! dependency on the exact types/functions this slice deletes
//! (`phoxal::catalog::Manifest`, `compare_catalogs`, `GenerateOptions`'s old
//! shape, ...), so leaving it untouched would not build. See the slice
//! report for the full rationale; the real "ci release-gate ADAPT" work (a
//! richer bounded guard, if one is wanted beyond what's here) remains open
//! for whichever slice owns `.github/workflows/*`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args as ClapArgs, Subcommand};
use phoxal::catalog::Catalog;

use crate::catalog::check::validate_coverage;
use crate::catalog::generate::workspace_relative_path;
use crate::catalog::verify::{validate_structure, verify_catalog_path};
use crate::release::upload::github_release_asset_names;
use crate::workspace::Workspace;

#[derive(Debug, Subcommand)]
pub enum Command {
    ReleaseGate(ReleaseGateArgs),
}

#[derive(Debug, ClapArgs)]
pub struct ReleaseGateArgs {
    #[arg(
        long,
        value_name = "PATH",
        default_value = "target/xtask/catalog/catalog.json"
    )]
    pub catalog: PathBuf,
    #[arg(long, value_name = "OWNER/REPO", env = "GITHUB_REPOSITORY")]
    pub repo: Option<String>,
    /// Confirm every filename this run's catalog entries place in
    /// `catalog.build.tag`'s release is actually uploaded there (design doc
    /// §4.4's "partial-upload guard"). Bounded to this run's own artifacts -
    /// carried-forward entries from earlier `build-*` releases are trusted
    /// by immutability and never re-checked. Requires `--repo`.
    #[arg(long)]
    pub verify_upload: bool,
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::ReleaseGate(args) => release_gate(args),
    }
}

fn release_gate(args: ReleaseGateArgs) -> Result<()> {
    let workspace = Workspace::discover()?;
    let catalog_path = workspace_relative_path(&workspace, &args.catalog);
    let catalog = verify_catalog_path(&catalog_path)?;
    // `verify_catalog_path` already runs `validate_structure(_, false)`, but
    // spell out the gate's checks explicitly rather than relying on that as
    // an implementation detail.
    validate_structure(&catalog, false)?;
    validate_coverage(&catalog, workspace.official_artifacts())?;

    if args.verify_upload {
        let repo = args
            .repo
            .as_deref()
            .context("--verify-upload requires --repo or GITHUB_REPOSITORY")?;
        verify_this_run_uploaded(repo, &catalog)?;
    }

    println!(
        "release gate passed: catalog {} covers {} artifact entries",
        catalog.schema,
        catalog.artifacts.len()
    );
    Ok(())
}

/// Filenames this run's own catalog entries expect to find in
/// `catalog.build.tag`'s GitHub release (identified by their `Blob.url`
/// pointing at that tag), cross-checked against `gh release view`'s actual
/// asset list. Older entries carried forward from a previous run point at a
/// different (already-immutable) release tag and are skipped.
fn verify_this_run_uploaded(repo: &str, catalog: &Catalog) -> Result<()> {
    let tag = &catalog.build.tag;
    let expected_prefix = format!("https://github.com/{repo}/releases/download/{tag}/");

    let mut expected_names = BTreeSet::new();
    for artifact in &catalog.artifacts {
        for blob in artifact.targets.values() {
            if let Some(name) = blob.url.strip_prefix(&expected_prefix) {
                expected_names.insert(name.to_string());
            }
        }
        if let Some(blob) = &artifact.assets {
            if let Some(name) = blob.url.strip_prefix(&expected_prefix) {
                expected_names.insert(name.to_string());
            }
        }
    }
    if expected_names.is_empty() {
        return Ok(());
    }

    let existing = github_release_asset_names(repo, tag)?;
    let missing: Vec<&str> = expected_names
        .iter()
        .filter(|name| !existing.contains(name.as_str()))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        bail!(
            "GitHub release {tag} is missing uploaded asset(s): {}",
            missing.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use phoxal::catalog::{Artifact, Blob, BuildProvenance, Heads};

    fn fixture_catalog(tag: &str) -> Catalog {
        let mut targets = BTreeMap::new();
        targets.insert(
            "x86_64-unknown-linux-gnu".to_string(),
            Blob {
                url: format!(
                    "https://github.com/phoxal/framework/releases/download/{tag}/phoxal-service-drive-v0.1.0-x86_64-unknown-linux-gnu.tar.zst"
                ),
                sha256: "a".repeat(64),
                size: 1,
            },
        );
        Catalog::new(
            BuildProvenance {
                tag: tag.to_string(),
                run_number: 1,
                run_id: 1,
                commit: "c".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
            vec![Artifact {
                package: "phoxal/service-drive".to_string(),
                version: "0.1.0".to_string(),
                targets,
                assets: None,
            }],
            Heads::empty(),
        )
    }

    #[test]
    fn verify_this_run_uploaded_ignores_carried_forward_entries() {
        // A catalog whose only entry points at a DIFFERENT (older) build tag
        // than `catalog.build.tag` has nothing to check for "this run".
        let catalog = fixture_catalog("build-old");
        let mut catalog = catalog;
        catalog.build.tag = "build-new".to_string();
        verify_this_run_uploaded("phoxal/framework", &catalog)
            .expect("no this-run assets to verify");
    }
}
