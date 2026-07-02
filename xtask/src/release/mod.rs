use anyhow::Result;
use clap::Subcommand;

pub mod discover;
pub mod package;

#[derive(Debug, Subcommand)]
pub enum Command {
    Discover(discover::Args),
    Package(package::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Discover(args) => discover::run(args),
        Command::Package(args) => package::run(args),
    }
}
