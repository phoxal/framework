use anyhow::Result;
use clap::Subcommand;

pub mod assets;
pub mod bump;
pub mod cut;
pub mod discover;
pub(crate) mod metadata;
pub mod package;
pub mod plan;

#[derive(Debug, Subcommand)]
pub enum Command {
    Discover(discover::Args),
    Bump(bump::Args),
    Cut(cut::Args),
    Package(package::Args),
    Assets(assets::Args),
    Plan(plan::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Discover(args) => discover::run(args),
        Command::Bump(args) => bump::run(args),
        Command::Cut(args) => cut::run(args),
        Command::Package(args) => package::run(args),
        Command::Assets(args) => assets::run(args),
        Command::Plan(args) => plan::run(args),
    }
}
