//! `cargo xtask coherence-check` - the release-PR gate for the deployment
//! coherence pass (coherence-gate design doc §4/§5, `organization`
//! `tmp/coherence-gate/readme.md`).
//!
//! Assembles the contract surface of every artifact in the whole official set
//! (`Workspace::official_artifacts()`, never just changed crates - design doc
//! §5, "a change to A can break a contract A shares with unchanged B") by
//! host-building each one and extracting its `#[derive(phoxal::Api)]`
//! metadata section (`crate::release::package::build_and_extract_metadata`),
//! then runs `phoxal::check::check_coherence` over the assembled set (§3:
//! per-participant pub/sub overlap + every-ask-matched).
//!
//! Wired as the required status check on release-plz's `chore(release)` PR
//! (`.github/workflows/coherence-check.yml`); "block" there means this
//! command exits non-zero.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use clap::Args as ClapArgs;

use phoxal::check::{
    CoherenceMismatch, CoherenceReport, ParticipantContractSurface, check_coherence,
};

use crate::release::package::build_and_extract_metadata;
use crate::workspace::{Workspace, require_nonempty_artifacts};

#[derive(Debug, ClapArgs)]
pub struct Args {}

pub fn run(_args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let artifacts = workspace.official_artifacts();
    require_nonempty_artifacts(artifacts)?;

    let mut surfaces = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        let meta = build_and_extract_metadata(&workspace, artifact).with_context(|| {
            format!(
                "failed to build {} and extract its API metadata for the coherence check",
                artifact.package
            )
        })?;
        surfaces.push(ParticipantContractSurface {
            participant_id: artifact.package.clone(),
            contracts: meta.contracts,
        });
    }

    let (report, lines) = evaluate(&surfaces);
    for line in &lines {
        println!("{line}");
    }

    if !report.is_ok() {
        bail!(
            "coherence check failed: {} mismatch(es) found across {} participant(s)",
            report.mismatches.len(),
            surfaces.len()
        );
    }

    Ok(())
}

/// Runs the pure `check_coherence` pass and renders its outcome as
/// human-readable diagnostic lines - split out from [`run`] so the
/// surface-assembly/report-formatting glue is unit-testable against synthetic
/// surfaces without a real host build (`check_coherence` itself already has
/// its own unit tests against the design doc's worked examples, `phoxal/src/check.rs`).
pub(crate) fn evaluate(surfaces: &[ParticipantContractSurface]) -> (CoherenceReport, Vec<String>) {
    let report = check_coherence(surfaces);
    let lines = if report.is_ok() {
        vec![format!(
            "coherence check passed: {} participant(s), no mismatches",
            surfaces.len()
        )]
    } else {
        report.mismatches.iter().map(format_mismatch).collect()
    };
    (report, lines)
}

/// One human-readable diagnostic line for a single mismatch: participant, the
/// `version::contract` join, the role/kind of mismatch, and the
/// disjoint/served version sets (coherence-gate design doc §3).
fn format_mismatch(mismatch: &CoherenceMismatch) -> String {
    match mismatch {
        CoherenceMismatch::PubSubDisjoint {
            participant_id,
            contract,
            subscribed,
            published,
        } => format!(
            "MISMATCH pub/sub: participant '{participant_id}' subscribes '{contract}' at \
             version(s) {{{}}}, but the in-set publisher(s) of '{contract}' only publish \
             version(s) {{{}}} - disjoint, so this participant cannot hear any in-set producer \
             of a contract that is demonstrably produced in-set",
            join_versions(subscribed),
            join_versions(published),
        ),
        CoherenceMismatch::UnservedAsk {
            participant_id,
            contract,
            version,
            served,
        } => format!(
            "MISMATCH ask/serve: participant '{participant_id}' asks '{contract}' at version \
             '{version}', but no in-set server answers that version (in-set served \
             version(s): {{{}}}) - every call on this ask permanently fails with \
             QueryError::Timeout",
            join_versions(served),
        ),
    }
}

fn join_versions(versions: &BTreeSet<String>) -> String {
    versions.iter().cloned().collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::participant::metadata::ParticipantMetaContract;

    fn meta_contract(
        role: &str,
        version: &str,
        contract: &str,
        external: bool,
    ) -> ParticipantMetaContract {
        ParticipantMetaContract {
            role: role.to_string(),
            version: version.to_string(),
            contract: contract.to_string(),
            external,
        }
    }

    fn surface(id: &str, contracts: Vec<ParticipantMetaContract>) -> ParticipantContractSurface {
        ParticipantContractSurface {
            participant_id: id.to_string(),
            contracts,
        }
    }

    #[test]
    fn coherent_set_passes_and_prints_a_one_line_ok() {
        let surfaces = vec![
            surface(
                "drive",
                vec![meta_contract("publish", "v1", "drive::Target", false)],
            ),
            surface(
                "mission",
                vec![meta_contract("subscribe", "v1", "drive::Target", false)],
            ),
        ];

        let (report, lines) = evaluate(&surfaces);

        assert!(report.is_ok());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("coherence check passed"));
        assert!(lines[0].contains("2 participant"));
    }

    #[test]
    fn disjoint_pub_sub_set_fails_with_a_diagnostic_line() {
        let surfaces = vec![
            surface(
                "drive",
                vec![meta_contract("publish", "v1", "drive::Target", false)],
            ),
            surface(
                "mission",
                vec![meta_contract("subscribe", "v2", "drive::Target", false)],
            ),
        ];

        let (report, lines) = evaluate(&surfaces);

        assert!(!report.is_ok());
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert!(line.contains("MISMATCH pub/sub"));
        assert!(line.contains("'mission'"));
        assert!(line.contains("'drive::Target'"));
        assert!(line.contains("v2"));
        assert!(line.contains("v1"));
    }

    #[test]
    fn unserved_ask_fails_with_a_diagnostic_line() {
        let surfaces = vec![
            surface(
                "asset",
                vec![meta_contract("serve", "v1", "asset::Get", false)],
            ),
            surface(
                "client",
                vec![meta_contract("ask", "v2", "asset::Get", false)],
            ),
        ];

        let (report, lines) = evaluate(&surfaces);

        assert!(!report.is_ok());
        let line = &lines[0];
        assert!(line.contains("MISMATCH ask/serve"));
        assert!(line.contains("'client'"));
        assert!(line.contains("'asset::Get'"));
        assert!(line.contains("v2"));
        assert!(line.contains("v1"));
    }

    #[test]
    fn external_marker_excuses_a_subscribe_mismatch_and_stays_coherent() {
        let surfaces = vec![
            surface(
                "drive",
                vec![meta_contract("publish", "v1", "drive::Target", false)],
            ),
            surface(
                "teleop",
                vec![meta_contract("subscribe", "v2", "drive::Target", true)],
            ),
        ];

        let (report, lines) = evaluate(&surfaces);

        assert!(report.is_ok());
        assert!(lines[0].contains("coherence check passed"));
    }

    #[test]
    fn empty_participant_set_is_coherent() {
        let (report, lines) = evaluate(&[]);
        assert!(report.is_ok());
        assert!(lines[0].contains("0 participant"));
    }
}
