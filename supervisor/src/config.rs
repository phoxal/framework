use clap::Parser;
use std::path::PathBuf;

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
pub(super) struct Cli {
    /// The compiled bundle directory whose execution to observe.
    #[arg(value_name = "BUNDLE_ROOT")]
    pub(super) bundle_root: PathBuf,

    /// Deployment namespace governed by router authorization.
    #[arg(long, value_name = "SCOPE")]
    pub(super) scope: String,

    /// Stable supervisor target within the deployment namespace.
    #[arg(long, value_name = "ID")]
    pub(super) supervisor_id: String,

    /// Explicit router endpoint for this execution.
    #[arg(long, value_name = "ENDPOINT")]
    pub(super) listen: Option<String>,

    /// Internal machine-readable readiness handoff for local orchestration.
    #[arg(long, value_name = "PATH", hide = true)]
    pub(super) ready_file: Option<PathBuf>,
}

const ABOUT: &str = "phoxal-supervisor - the Phoxal Framework execution observer";

const LONG_ABOUT: &str = "\
phoxal-supervisor - the Phoxal Framework execution observer

<BUNDLE_ROOT> is a compiled bundle directory: manifest.json, assets/, and bin/.
Build one with `cargo phoxal build`. The required --scope and --supervisor-id
select the installed public routing namespace; neither value comes from the
bundle. The supervisor runs the router, reports which of the robot's runtimes
are present, and retains their logs and telemetry. It admits and launches the selected runtime processes.

`--version` reports this supervisor package's own version. The linked framework version is available through an authenticated session information request.";
