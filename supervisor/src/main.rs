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
