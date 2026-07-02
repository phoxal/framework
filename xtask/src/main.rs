use anyhow::Result;
use clap::{Parser, Subcommand};

mod release;
mod workspace;

#[derive(Debug, Parser)]
#[command(about = "Phoxal workspace automation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Release {
        #[command(subcommand)]
        command: release::Command,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Release { command } => release::run(command),
    }
}
