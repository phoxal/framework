//! The framework workspace's command runner, reached as `cargo xtask <verb>`.
//!
//! Today it carries one verb group: `compatibility`, which compares the
//! contract surfaces this workspace declares, and the authored documents this
//! workspace reads, against the latest published framework train, and says what
//! release those changes require.
//!
//! The runner depends on no framework crate. Both sides of a comparison are
//! read out of separately compiled probe projects, so building the checker
//! never builds the runtime stack it checks, and the checker can never report
//! the surface it was itself compiled against.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use semver::Version;

mod check;
mod index;
mod probe;
mod readiness;
mod rehearsal;
mod release;
mod runbook;
mod source;
mod surface;
mod toolchain;

use crate::check::CompatibilityCheck;
use crate::index::SparseIndex;
use crate::probe::ProbeSurfaces;
use crate::readiness::V1Readiness;
use crate::rehearsal::Rehearsal;
use crate::source::SourceProbe;
use crate::surface::CompatibilityImpact;

/// The version this workspace would release next.
///
/// The runner inherits `version.workspace = true`, so its own package version
/// is the workspace train version, and reading it here keeps the train stated
/// in exactly one place.
const WORKSPACE_VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace this runner is part of.
///
/// The root is the runner's own parent directory, so every verb finds the
/// crates it reads no matter where it was invoked from.
pub(crate) fn workspace_root() -> Result<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("the runner's manifest directory has no workspace parent")?
        .to_path_buf())
}

fn run() -> Result<ExitCode> {
    let Verb::Compatibility(verb) = Cli::parse().verb;
    match verb {
        CompatibilityVerb::Report { options } => compare(&options, false),
        CompatibilityVerb::CheckRelease { options } => compare(&options, true),
        CompatibilityVerb::RehearseV1 => {
            let report = Rehearsal::new(workspace_root()?).run()?;
            println!("{report}");
            Ok(report.exit_code())
        }
        CompatibilityVerb::V1Readiness {
            allow_rust_version_raise,
        } => {
            let report = V1Readiness::new(workspace_root()?, allow_rust_version_raise).run()?;
            println!("{report}");
            Ok(report.exit_code())
        }
    }
}

/// Compare this workspace against the published train, and gate the version on
/// the result when the caller asked for a gate.
fn compare(options: &ComparisonOptions, gates_the_release: bool) -> Result<ExitCode> {
    let report = CompatibilityCheck::new(
        SparseIndex::crates_io(),
        ProbeSurfaces::for_workspace()?,
        SourceProbe::for_workspace()?,
        Version::parse(WORKSPACE_VERSION)?,
        toolchain::workspace_floor(&workspace_root()?)?,
    )
    .run(options.declared_impact)?;
    println!("{report}");

    if !gates_the_release {
        return Ok(ExitCode::SUCCESS);
    }
    let Some(shortfall) = report.release_shortfall() else {
        return Ok(ExitCode::SUCCESS);
    };
    eprintln!("error: {shortfall}");
    Ok(ExitCode::FAILURE)
}

/// The workspace's own tasks.
#[derive(Debug, Parser)]
#[command(
    bin_name = "cargo xtask",
    about = "Workspace tasks for the Phoxal framework."
)]
struct Cli {
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Debug, Subcommand)]
enum Verb {
    /// Compare this workspace's contract surfaces against the published train.
    #[command(subcommand)]
    Compatibility(CompatibilityVerb),
}

#[derive(Debug, Subcommand)]
enum CompatibilityVerb {
    /// Report what changed and what release it requires.
    ///
    /// Succeeds whatever it finds, so an ordinary pull request sees the impact
    /// of its own change without being gated on a version it is not setting.
    Report {
        #[command(flatten)]
        options: ComparisonOptions,
    },
    /// Report what changed and fail unless the workspace version is a
    /// sufficient release over the published train.
    ///
    /// Over-releasing passes: only a version that under-states what changed is
    /// refused.
    CheckRelease {
        #[command(flatten)]
        options: ComparisonOptions,
    },
    /// Drill the Stable-line semantics this workspace has not reached yet.
    ///
    /// The framework is pre-1.0, so every post-1.0 code path runs only inside
    /// `cargo test`. This runs the whole matrix as one command, reaching no
    /// registry, so the paths are watched behaving before the day they matter.
    #[command(name = "rehearse-v1")]
    RehearseV1,
    /// Check everything that has to be true before this workspace releases a
    /// 1.0, and name what is left for a human.
    #[command(name = "v1-readiness")]
    V1Readiness {
        /// Accept a toolchain floor higher than the published train's.
        ///
        /// A raised floor breaks every consumer still on the previous
        /// toolchain, so the gate refuses it until someone says it is
        /// deliberate. The release still has to be at least a minor.
        #[arg(long)]
        allow_rust_version_raise: bool,
    },
}

#[derive(Args, Debug)]
struct ComparisonOptions {
    /// Raise the impact to at least this level.
    ///
    /// The checker proves structure. It sees a renamed field, a removed
    /// endpoint, a changed launch argument; it cannot see a meaning that
    /// changed under a shape that did not - a unit reinterpreted, a frame
    /// convention flipped, a field that starts carrying a different quantity.
    /// Declare such a break here and the required release rises with it.
    ///
    /// It only ever raises: a level below what the surfaces themselves show is
    /// ignored, so nothing can talk a real break down. Stating the semantic
    /// change in the source and in review remains the author's job.
    #[arg(long, value_enum, default_value_t = CompatibilityImpact::Unchanged)]
    declared_impact: CompatibilityImpact,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use clap::CommandFactory;

    use super::*;

    /// The runner's own package version is the train version it reports
    /// against, which only holds while it inherits the workspace one.
    #[test]
    fn the_runner_carries_the_workspace_train_version() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the runner sits in the workspace");
        let manifest =
            fs::read_to_string(root.join("Cargo.toml")).expect("the root manifest reads");
        let declared = manifest
            .lines()
            .skip_while(|line| line.trim() != "[workspace.package]")
            .find_map(|line| line.trim().strip_prefix("version = "))
            .map(|value| value.trim_matches('"').to_owned())
            .expect("the workspace declares a train version");
        assert_eq!(declared, WORKSPACE_VERSION);
    }

    fn parse(arguments: [&str; 4]) -> CompatibilityVerb {
        let Verb::Compatibility(verb) =
            Cli::try_parse_from(arguments.iter().take_while(|argument| !argument.is_empty()))
                .expect("the verb parses")
                .verb;
        verb
    }

    /// The two comparing subcommands differ only in whether they gate the
    /// version, so both carry the escalation flag.
    #[test]
    fn both_comparing_verbs_accept_a_declared_impact() {
        for verb in ["report", "check-release"] {
            let parsed = Cli::try_parse_from([
                "cargo xtask",
                "compatibility",
                verb,
                "--declared-impact",
                "breaking",
            ])
            .expect("the verb parses");
            let Verb::Compatibility(
                CompatibilityVerb::Report { options } | CompatibilityVerb::CheckRelease { options },
            ) = parsed.verb
            else {
                panic!("{verb} is a comparing verb");
            };
            assert_eq!(options.declared_impact, CompatibilityImpact::Breaking);
        }
    }

    /// Nothing is escalated unless a caller says so.
    #[test]
    fn the_declared_impact_defaults_to_unchanged() {
        let CompatibilityVerb::Report { options } =
            parse(["cargo xtask", "compatibility", "report", ""])
        else {
            panic!("report parses as the reporting verb");
        };
        assert_eq!(options.declared_impact, CompatibilityImpact::Unchanged);
    }

    /// The two v1 verbs are spelled the way the runbook and CI spell them.
    #[test]
    fn the_v1_verbs_parse_under_their_documented_names() {
        assert!(matches!(
            parse(["cargo xtask", "compatibility", "rehearse-v1", ""]),
            CompatibilityVerb::RehearseV1
        ));
        assert!(matches!(
            parse(["cargo xtask", "compatibility", "v1-readiness", ""]),
            CompatibilityVerb::V1Readiness {
                allow_rust_version_raise: false
            }
        ));
        assert!(matches!(
            parse([
                "cargo xtask",
                "compatibility",
                "v1-readiness",
                "--allow-rust-version-raise"
            ]),
            CompatibilityVerb::V1Readiness {
                allow_rust_version_raise: true
            }
        ));
    }

    #[test]
    fn the_command_definition_is_well_formed() {
        Cli::command().debug_assert();
    }
}
