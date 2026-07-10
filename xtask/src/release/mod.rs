use anyhow::Result;
use clap::Subcommand;

pub mod discover;
pub mod github;
pub(crate) mod metadata;
pub mod package;
pub mod plan;
pub mod upload;

#[derive(Debug, Subcommand)]
pub enum Command {
    Discover(discover::Args),
    Package(package::Args),
    Plan(plan::Args),
    Upload(upload::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Discover(args) => discover::run(args),
        Command::Package(args) => package::run(args),
        Command::Plan(args) => plan::run(args),
        Command::Upload(args) => upload::run(args),
    }
}
