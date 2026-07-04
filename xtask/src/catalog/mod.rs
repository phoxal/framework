use anyhow::Result;
use clap::Subcommand;

pub mod check;
pub mod generate;
pub mod publish;
pub(crate) mod schema;
pub mod verify;

#[derive(Debug, Subcommand)]
pub enum Command {
    Generate(generate::Args),
    Check(check::Args),
    Publish(publish::Args),
    Verify(verify::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Generate(args) => generate::run(args),
        Command::Check(args) => check::run(args),
        Command::Publish(args) => publish::run(args),
        Command::Verify(args) => verify::run(args),
    }
}
