//! The framework workspace's command runner, reached as `cargo xtask <verb>`.
//!
//! The runner owns only workspace-wide invariants that no individual crate can
//! enforce, such as package layout, dependency direction, feature boundaries,
//! registry publication policy, and deleted-surface checks.
//!
//! Release sizing and a framework-wide compatibility baseline do not belong in
//! this binary: Cargo packages have independent versions, while Protobuf and
//! public runtime contracts are checked by their owning packages and Buf.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod policy;

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
pub(crate) fn workspace_root() -> Result<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("the runner's manifest directory has no workspace parent")?
        .to_path_buf())
}

fn run() -> Result<ExitCode> {
    match Cli::parse().verb {
        Verb::Policy => {
            let report = policy::run()?;
            println!("{report}");
            Ok(report.exit_code())
        }
    }
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
    /// Enforce the rules the workspace must obey as a whole.
    ///
    /// Reads `cargo metadata`, the filesystem and Git only, so it needs no
    /// framework crate and builds nothing it judges.
    Policy,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_policy_gate_parses_under_its_documented_name() {
        assert!(matches!(
            Cli::try_parse_from(["cargo xtask", "policy"])
                .expect("the verb parses")
                .verb,
            Verb::Policy
        ));
    }

    #[test]
    fn compatibility_commands_are_not_part_of_the_runner() {
        let error = Cli::try_parse_from(["cargo xtask", "compatibility"])
            .expect_err("the retired compatibility verb must not parse")
            .to_string();
        assert!(error.contains("unrecognized subcommand"), "{error}");
    }

    #[test]
    fn the_command_definition_is_well_formed() {
        Cli::command().debug_assert();
    }
}
