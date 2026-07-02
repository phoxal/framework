use anyhow::Result;
use clap::Subcommand;

pub mod sync_features;

#[derive(Debug, Subcommand)]
pub enum Command {
    SyncFeatures(sync_features::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::SyncFeatures(args) => sync_features::run(args),
    }
}
