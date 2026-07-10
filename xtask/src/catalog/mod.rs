use anyhow::Result;
use clap::Subcommand;

pub mod check;
pub mod generate;
pub mod model;
pub mod verify;

#[derive(Debug, Subcommand)]
pub enum Command {
    Generate(generate::Args),
    Check(check::Args),
    Verify(verify::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Generate(args) => generate::run(args),
        Command::Check(args) => check::run(args),
        Command::Verify(args) => verify::run(args),
    }
}
