use anyhow::Result;
use clap::Parser;

mod coherence;
mod metadata;
mod workspace;

/// Workspace automation, now a single job.
///
/// The release verbs (`package`, `assets`, `suite`, `verify`) are gone with the
/// archive-and-suite release model: Cargo publishes the train, so there is no
/// Phoxal-owned release tool left to run (organization#951, Decision 12 of the
/// release flow). What remains is the one check Cargo cannot do for us.
#[derive(Debug, Parser)]
#[command(about = "Phoxal workspace automation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// The deployment coherence gate over the whole official artifact set:
    /// host-builds every official artifact, extracts its
    /// `#[derive(phoxal::Api)]` contract surface, and runs
    /// `phoxal::check::check_coherence`. Wired as the required status check
    /// on release-plz's `chore(release)` PR.
    CoherenceCheck(coherence::Args),
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::CoherenceCheck(args) => coherence::run(args),
    }
}
