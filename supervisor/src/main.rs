//! `phoxal-supervisor` - the Phoxal Framework execution observer.
//!
//! ```text
//! phoxal-supervisor <BUNDLE_ROOT>
//! ```
//!
//! That is the entire command line, and deliberately so.
//! There is no `run`, `start`, `attach`, `stop`, `status`, `log`, `build`,
//! `install`, `deploy`, `doctor`, or `upgrade` subcommand; no `--drivers`,
//! `--driver`, or simulation flag; and no execution options of any kind. The
//! bundle root is the supervisor's complete input, and everything it needs is
//! in the `manifest.json` inside it.
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
//! reports the framework-owned executable's package version. It is diagnostic:
//! a client that needs the framework train this supervisor speaks asks
//! `supervisor/connect`, the one endpoint frozen across every framework line.
//!
//! This process launches nothing. It runs the router the graph meets on,
//! watches which of the robot's expected runtimes are present, retains their
//! logs and telemetry, serves the bundle, and can reboot or power off its host.
//! Whoever started a runtime - `phoxal` locally, systemd on a device - is what
//! restarts or stops it.

mod router;
mod supervisor;
mod systemd;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

/// Observe one compiled bundle's execution.
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
    /// The compiled bundle directory whose execution to observe.
    #[arg(value_name = "BUNDLE_ROOT")]
    bundle_root: PathBuf,
}

const ABOUT: &str = "phoxal-supervisor - the Phoxal Framework execution observer";

const LONG_ABOUT: &str = "\
phoxal-supervisor - the Phoxal Framework execution observer

<BUNDLE_ROOT> is a compiled bundle directory: manifest.json, assets/, and bin/.
Build one with `phoxal build`. The supervisor runs the router, reports which of
the robot's runtimes are present, and retains their logs and telemetry. It
launches no runtime and takes no other options.

`--version` reports this supervisor package's own version. The framework train
this supervisor speaks is answered by the `supervisor/connect` endpoint.";

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

/// The supervisor's own diagnostics, on stderr.
///
/// Under systemd stderr is the journal; interactively it is the terminal the
/// operator launched from, and locally `phoxal` captures it to a file. Either
/// way it is the only channel the supervisor has before the bus exists, and it
/// is deliberately not the bus log stream: the process that retains everyone
/// else's records cannot also be a client of its own retention.
///
/// The default quietens the transport rather than the supervisor: Zenoh's own
/// info-level chatter says nothing about this robot, and the unix-socket link
/// warns on every ordinary participant disconnect. `RUST_LOG` replaces the
/// whole default when it is set.
fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
}

const DEFAULT_LOG_FILTER: &str = "info,zenoh=warn,zenoh_link_unixsock_stream=error";
