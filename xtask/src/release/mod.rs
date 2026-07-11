use anyhow::Result;
use clap::Subcommand;

pub mod assets;
pub mod bootstrap_tags;
pub mod discover;
pub(crate) mod metadata;
pub mod package;
pub mod plan;

#[derive(Debug, Subcommand)]
pub enum Command {
    Discover(discover::Args),
    /// Restore missing git-only version baselines from a published catalog.
    BootstrapTags(bootstrap_tags::Args),
    Package(package::Args),
    Assets(assets::Args),
    Plan(plan::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::Discover(args) => discover::run(args),
        Command::BootstrapTags(args) => bootstrap_tags::run(args),
        Command::Package(args) => package::run(args),
        Command::Assets(args) => assets::run(args),
        Command::Plan(args) => plan::run(args),
    }
}
