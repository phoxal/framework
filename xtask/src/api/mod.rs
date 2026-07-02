use anyhow::Result;
use clap::Subcommand;

pub mod freeze_shapes;
pub mod manifest;
pub mod preview_impact;
pub mod promotion_gate;
pub mod schema_diff;
pub mod sync_features;

#[derive(Debug, Subcommand)]
pub enum Command {
    SyncFeatures(sync_features::Args),
    SchemaDiff(schema_diff::Args),
    PreviewImpact(preview_impact::Args),
    PromotionGate(promotion_gate::Args),
    FreezeShapes(freeze_shapes::Args),
}

pub fn run(command: Command) -> Result<()> {
    match command {
        Command::SyncFeatures(args) => sync_features::run(args),
        Command::SchemaDiff(args) => schema_diff::run(args),
        Command::PreviewImpact(args) => preview_impact::run(args),
        Command::PromotionGate(args) => promotion_gate::run(args),
        Command::FreezeShapes(args) => freeze_shapes::run(args),
    }
}
