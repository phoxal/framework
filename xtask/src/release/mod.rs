use anyhow::Result;
use clap::Subcommand;

pub mod bump;
pub mod cut;
pub mod discover;
pub mod github;
pub mod package;
pub mod plan;
pub mod publish;
pub mod upload;

#[derive(Debug, Subcommand)]
pub enum Command {
    Bump(bump::Args),
    Cut(cut::Args),
    Discover(discover::Args),
    Package(package::Args),
    Plan(plan::Args),
    Publish(publish::Args),
    Upload(upload::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Bump(args) => bump::run(args),
        Command::Cut(args) => cut::run(args),
        Command::Discover(args) => discover::run(args),
        Command::Package(args) => package::run(args),
        Command::Plan(args) => plan::run(args),
        Command::Publish(args) => publish::run(args),
        Command::Upload(args) => upload::run(args),
    }
}
