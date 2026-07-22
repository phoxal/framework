use anyhow::Result;
use clap::{Parser, Subcommand};

mod coherence;
mod release;
mod workspace;

#[derive(Debug, Parser)]
#[command(about = "Phoxal workspace automation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Release {
        #[command(subcommand)]
        command: release::Command,
    },
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
        Command::Release { command } => release::run(command),
        Command::CoherenceCheck(args) => coherence::run(args),
    }
}
