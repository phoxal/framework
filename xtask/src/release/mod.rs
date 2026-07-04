use anyhow::Result;
use clap::Subcommand;

pub mod bump;
pub mod discover;
pub mod package;
pub mod sync_config;

#[derive(Debug, Subcommand)]
pub enum Command {
    Bump(bump::Args),
    Discover(discover::Args),
    Package(package::Args),
    SyncConfig(sync_config::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Bump(args) => bump::run(args),
        Command::Discover(args) => discover::run(args),
        Command::Package(args) => package::run(args),
        Command::SyncConfig(args) => sync_config::run(args),
    }
}
