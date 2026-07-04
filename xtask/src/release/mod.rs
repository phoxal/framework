use anyhow::Result;
use clap::Subcommand;

pub mod bump;
pub mod discover;
pub mod package;
pub mod plan;
pub mod sync_config;
pub mod upload;

#[derive(Debug, Subcommand)]
pub enum Command {
    Bump(bump::Args),
    Discover(discover::Args),
    Package(package::Args),
    Plan(plan::Args),
    SyncConfig(sync_config::Args),
    Upload(upload::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Bump(args) => bump::run(args),
        Command::Discover(args) => discover::run(args),
        Command::Package(args) => package::run(args),
        Command::Plan(args) => plan::run(args),
        Command::SyncConfig(args) => sync_config::run(args),
        Command::Upload(args) => upload::run(args),
    }
}
