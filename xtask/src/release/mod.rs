use anyhow::Result;
use clap::Subcommand;

pub mod assets;
pub(crate) mod metadata;
pub mod package;
pub mod verify;

#[derive(Debug, Subcommand)]
pub enum Command {
    Package(package::Args),
    Assets(assets::Args),
    Verify(verify::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Package(args) => package::run(args),
        Command::Assets(args) => assets::run(args),
        Command::Verify(args) => verify::run(args),
    }
}
