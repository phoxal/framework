//! The framework workspace's command runner, reached as `cargo xtask <verb>`.
//!
//! Today it carries one verb group: `compatibility`, which compares the
//! contract surfaces this workspace declares against the latest published
//! framework train and says what release those changes require.
//!
//! The runner depends on no framework crate. Both sides of a comparison are
//! read out of separately compiled probe projects, so building the checker
//! never builds the runtime stack it checks, and the checker can never report
//! the surface it was itself compiled against.

use std::process::ExitCode;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use semver::Version;

mod check;
mod index;
mod probe;
mod release;
mod surface;

use crate::check::CompatibilityCheck;
use crate::index::SparseIndex;
use crate::probe::ProbeSurfaces;
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

fn run() -> Result<ExitCode> {
    let Verb::Compatibility(verb) = Cli::parse().verb;
    let report = CompatibilityCheck::new(
        SparseIndex::crates_io(),
        ProbeSurfaces::for_workspace()?,
        Version::parse(WORKSPACE_VERSION)?,
    )
    .run(verb.options().declared_impact)?;
    println!("{report}");

    if !verb.gates_the_release() {
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
}

impl CompatibilityVerb {
    fn options(&self) -> &ComparisonOptions {
        match self {
            Self::Report { options } | Self::CheckRelease { options } => options,
        }
    }

    /// Whether this verb fails when the workspace version under-states the
    /// change it carries.
    fn gates_the_release(&self) -> bool {
        match self {
            Self::Report { .. } => false,
            Self::CheckRelease { .. } => true,
        }
    }
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

    /// The two subcommands differ only in whether they gate the version, so
    /// both carry the escalation flag.
    #[test]
    fn both_compatibility_verbs_accept_a_declared_impact() {
        for verb in ["report", "check-release"] {
            let parsed = Cli::try_parse_from([
                "cargo xtask",
                "compatibility",
                verb,
                "--declared-impact",
                "breaking",
            ])
            .expect("the verb parses");
            let Verb::Compatibility(verb) = parsed.verb;
            assert_eq!(
                verb.options().declared_impact,
                CompatibilityImpact::Breaking
            );
        }
    }

    /// Nothing is escalated unless a caller says so.
    #[test]
    fn the_declared_impact_defaults_to_unchanged() {
        let parsed = Cli::try_parse_from(["cargo xtask", "compatibility", "report"])
            .expect("the verb parses");
        let Verb::Compatibility(verb) = parsed.verb;
        assert_eq!(
            verb.options().declared_impact,
            CompatibilityImpact::Unchanged
        );
    }

    #[test]
    fn the_command_definition_is_well_formed() {
        Cli::command().debug_assert();
    }
}
