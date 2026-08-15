//! `phoxal-supervisor` - the Phoxal Framework execution supervisor.
//!
//! ```text
//! phoxal-supervisor <BUNDLE_ROOT>
//! ```
//!
//! That is the entire command line, and deliberately so.
//! There is no `run`, `start`, `attach`, `stop`, `status`, `log`, `build`,
//! `install`, `deploy`, `doctor`, or `upgrade` subcommand; no `--drivers`,
//! `--driver`, or simulation flag; and no execution options of any kind. Clock
//! and participant selection are already written into the finalized manifest by
//! whoever built the bundle, so the bundle root is the supervisor's complete
//! input.
//!
//! `clap` parses that one operand. It is not here to advertise a surface this
//! binary does not have - there is still nothing to choose - but because the
//! surface *is* one operand plus the two conventional non-executing flags, and
//! those are exactly what clap already does correctly: `-h/--help` and
//! `-V/--version` in their standard shape on stdout, and strict rejection of a
//! missing operand, a second operand, or any flag this binary does not have,
//! with the error on stderr and exit code 2. Hand-rolling that rejection buys
//! nothing and risks getting a convention subtly wrong.
//!
//! The one non-executing invocation is `--version`. It exists because `phoxal`
//! reports the framework-owned executable's package version. It is diagnostic,
//! not an execution option or a compatibility gate: bundle compatibility comes
//! from the framework train its artifacts carry, which the supervisor reads
//! from the bundle itself.
//!
//! Everything an operator does *to* a running execution goes through the
//! supervisor API on the bus - `phoxal attach`, `phoxal status`, `phoxal stop`
//! - not through a second invocation of this binary.

mod model;
mod process;
mod router;
mod state;
mod supervisor;
mod systemd;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

/// Run one compiled bundle.
///
/// `version` is spelled out rather than left to clap's `#[command(version)]`
/// shorthand so the printed line is unambiguously this package's version, and
/// `name` is fixed so it stays `phoxal-supervisor <version>` however the binary
/// was invoked.
#[derive(Debug, Parser)]
#[command(
    name = "phoxal-supervisor",
    version = env!("CARGO_PKG_VERSION"),
    about = ABOUT,
    long_about = LONG_ABOUT,
)]
struct Cli {
    /// The compiled bundle directory to validate and execute.
    #[arg(value_name = "BUNDLE_ROOT")]
    bundle_root: PathBuf,
}

const ABOUT: &str = "phoxal-supervisor - the Phoxal Framework execution supervisor";

const LONG_ABOUT: &str = "\
phoxal-supervisor - the Phoxal Framework execution supervisor

<BUNDLE_ROOT> is a compiled bundle directory: runtime.json, assets/, and bin/.
Build one with `phoxal build`. The supervisor validates and executes it; it never
builds, and it takes no other options - the clock and the participant set are
already written into runtime.json.

`--version` reports this supervisor package's own version. Bundle compatibility
uses the framework contract train, never this diagnostic product version.";

/// Multi-thread: Zenoh refuses to run on Tokio's current-thread scheduler, and
/// the router runs in this process.
#[tokio::main]
async fn main() -> ExitCode {
    // Parse first: help, version, and misuse all end the process without ever
    // needing a subscriber installed.
    let cli = Cli::parse();
    init_tracing();
    match supervisor::run(&cli.bundle_root).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Stderr is the supervisor's diagnostic channel under systemd,
            // where it is the journal. One rendered chain, not a panic.
            eprintln!("phoxal-supervisor: {error:#}");
            ExitCode::from(1)
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Under systemd, stderr is the journal; interactively it is the terminal
    // the operator launched from. Either way it is the only diagnostic channel
    // the supervisor has before the bus exists.
    // `JOURNAL_STREAM` means stderr is the journal, which does not render
    // escape codes - only a real terminal gets colour.
    let ansi = std::env::var_os("JOURNAL_STREAM").is_none()
        && std::io::IsTerminal::is_terminal(&std::io::stderr());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(ansi)
        .init();
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::CommandFactory;
    use clap::Parser;
    use clap::error::ErrorKind;

    use super::Cli;

    fn parse<const N: usize>(values: [&str; N]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("phoxal-supervisor").chain(values))
    }

    /// The declared surface is one required operand, and nothing else.
    #[test]
    fn the_parser_is_valid_for_exactly_one_bundle_root() {
        Cli::command().debug_assert();

        let cli = parse([".phoxal/release/bundle"]).expect("one operand is the whole surface");
        assert_eq!(cli.bundle_root, PathBuf::from(".phoxal/release/bundle"));

        // There are no subcommands to shadow an operand, so a directory that
        // happens to be named `run` is a bundle root like any other.
        let cli = parse(["run"]).expect("`run` is a directory name, not a subcommand");
        assert_eq!(cli.bundle_root, PathBuf::from("run"));
    }

    /// Help and version are successful non-executing invocations: clap reports
    /// them as errors only so the caller knows not to run anything.
    #[test]
    fn help_and_version_are_successful_non_executing_invocations() {
        for (arguments, kind) in [
            (["-h"], ErrorKind::DisplayHelp),
            (["--help"], ErrorKind::DisplayHelp),
            (["-V"], ErrorKind::DisplayVersion),
            (["--version"], ErrorKind::DisplayVersion),
        ] {
            let error = parse(arguments).expect_err("clap reports these as non-executing");
            assert_eq!(error.kind(), kind, "{arguments:?}");
            assert_eq!(error.exit_code(), 0, "{arguments:?}");
            assert!(!error.use_stderr(), "{arguments:?}");
        }
    }

    /// Every subcommand and obsolete flag this entry point once carried is
    /// gone, so anything shaped like one is a misuse with the conventional
    /// usage-error exit code rather than something this binary quietly ignores.
    #[test]
    fn everything_other_than_one_bundle_root_is_a_usage_error() {
        for misuse in [
            vec![],
            vec!["one", "two"],
            vec!["--drivers", "off"],
            vec!["--offline"],
            vec![".phoxal/release/bundle", "--drivers=off"],
        ] {
            let error = Cli::try_parse_from(
                std::iter::once("phoxal-supervisor").chain(misuse.iter().copied()),
            )
            .expect_err(&format!("{misuse:?} is a misuse"));
            assert_eq!(error.exit_code(), 2, "{misuse:?}");
            assert!(error.use_stderr(), "{misuse:?}");
        }
    }

    /// The diagnostic line names the framework-owned executable and its exact
    /// package version. `phoxal` matches this shape when it reports the
    /// supervisor it found.
    #[test]
    fn the_version_line_names_the_supervisor_package() {
        assert_eq!(
            Cli::command().render_version(),
            format!("phoxal-supervisor {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}
