use crate::util::init_tracing;
use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use dotenvy::Error as DotenvError;

use crate::runtime::RobotRuntimeArgs;
use crate::runtime::step::{Runtime, RuntimeProcess};

#[derive(Debug, Parser)]
#[command(about = "Robot runtime process")]
struct Cli<E: Args> {
    #[command(flatten)]
    common: RobotRuntimeArgs,

    #[command(flatten)]
    extra: E,

    #[command(subcommand)]
    command: Subcmd,
}

#[derive(Debug, Subcommand)]
enum Subcmd {
    /// Run the runtime's steady-state loop.
    Run,
    /// Run a single scenario this runtime owns.
    Scenario {
        /// Scenario name; matches a `ScenarioDescriptor::name`.
        #[arg(long)]
        name: String,
    },
    /// List the scenarios this runtime owns.
    Scenarios {
        #[command(subcommand)]
        action: ScenariosCmd,
    },
}

#[derive(Debug, Subcommand)]
enum ScenariosCmd {
    /// Print scenarios as a JSON array (machine-readable) by default.
    List {
        /// Print human-readable `name\tsummary` instead of JSON.
        #[arg(long)]
        plain: bool,
    },
}

pub async fn execute<R: Runtime>() -> Result<()> {
    match dotenvy::dotenv() {
        Ok(_) => {}
        Err(DotenvError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("failed to load .env"),
    }

    init_tracing()?;

    let cli = Cli::<R::Args>::parse();
    match cli.command {
        Subcmd::Run => run::<R>(cli.common, cli.extra).await,
        Subcmd::Scenario { name } => R::run_scenario(&name, &cli.common, &cli.extra).await,
        Subcmd::Scenarios {
            action: ScenariosCmd::List { plain },
        } => {
            let descriptors = R::scenarios();
            if plain {
                for descriptor in descriptors {
                    println!("{}\t{}", descriptor.name, descriptor.summary);
                }
            } else {
                let json =
                    serde_json::to_string(descriptors).context("failed to serialize scenarios")?;
                println!("{json}");
            }
            Ok(())
        }
    }
}

async fn run<R: Runtime>(common: RobotRuntimeArgs, extra: R::Args) -> Result<()> {
    let config = R::config(&extra, &common)?;
    let period = R::clock_period(&config);
    let bus = common.connect_bus().await?;
    RuntimeProcess::new(&bus, common.simulation, period)
        .run::<R>(config)
        .await
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Cli;
    use crate::runtime::step::EmptyArgs;

    #[test]
    fn parses_run_subcommand_with_empty_extra_args() {
        let result = Cli::<EmptyArgs>::try_parse_from([
            "bin",
            "--robot-config",
            "fixture/robot/rgbd-imu-diff-drive",
            "run",
        ]);

        assert!(result.is_ok(), "{result:?}");
    }
}
