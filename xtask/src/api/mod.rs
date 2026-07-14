use anyhow::Result;
use clap::Subcommand;

pub mod frozen_version;

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Block a release-plz release PR if a released (non-`preview`)
    /// `phoxal_api_tree!` version span changed since the crates.io
    /// baseline commit.
    FrozenVersionCheck(frozen_version::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::FrozenVersionCheck(args) => frozen_version::run(args),
    }
}
