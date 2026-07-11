use anyhow::Result;
use clap::Subcommand;

pub mod frozen_generation;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Block a release-plz release PR if a released (non-`preview`)
    /// `phoxal_api_tree!` generation span changed since the crates.io
    /// baseline commit.
    FrozenGenerationCheck(frozen_generation::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::FrozenGenerationCheck(args) => frozen_generation::run(args),
    }
}
