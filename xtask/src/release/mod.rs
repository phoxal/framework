use anyhow::Result;
use clap::Subcommand;

pub mod assets;
pub(crate) mod metadata;
pub mod package;
pub mod suite;
pub mod verify;

#[derive(Debug, Subcommand)]
pub enum Command {
    Package(package::Args),
    Assets(assets::Args),
    Suite(suite::Args),
    Verify(verify::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Package(args) => package::run(args),
        Command::Assets(args) => assets::run(args),
        Command::Suite(args) => suite::run(args),
        Command::Verify(args) => verify::run(args),
    }
}
