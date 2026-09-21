#![allow(dead_code, non_camel_case_types)]

#[path = "../scenarios/forward_turn_stop.rs"]
mod forward_turn_stop;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "phoxal-scenarios",
    about = "List or run authored Phoxal scenarios"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    List,
    Run { scenario: String },
}

fn main() -> phoxal::Result<()> {
    match Cli::parse().command.unwrap_or(Command::List) {
        Command::List => {
            for scenario in
                phoxal::scenario::list_scenarios().map_err(|error| phoxal::anyhow!("{error}"))?
            {
                println!("{}\t{}", scenario.name, scenario.module_path);
            }
            Ok(())
        }
        Command::Run { scenario } => phoxal::scenario::__harness::run_harness_case(&scenario)
            .map_err(|error| phoxal::anyhow!("scenario `{scenario}` case host failed: {error:#}")),
    }
}
