use anyhow::Result;
use clap::Args as ClapArgs;
use serde::Serialize;

use crate::api::manifest::{self, ContractDiff};
use crate::workspace::Workspace;

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(value_name = "FROM_GEN")]
    pub from_gen: String,
    #[arg(value_name = "TO_GEN")]
    pub to_gen: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct SchemaDiffReport {
    from_generation: String,
    to_generation: String,
    changed_contracts: Vec<ContractDiff>,
}

pub fn run(args: Args) -> Result<()> {
    let workspace = Workspace::discover()?;
    let api_manifest = manifest::load_from_workspace(&workspace)?;
    let changed_contracts = manifest::diff_contracts(&api_manifest, &args.from_gen, &args.to_gen)?;
    let report = SchemaDiffReport {
        from_generation: args.from_gen,
        to_generation: args.to_gen,
        changed_contracts,
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_human_report(&report);
    }

    Ok(())
}

fn print_human_report(report: &SchemaDiffReport) {
    println!(
        "schema diff {} -> {}",
        report.from_generation, report.to_generation
    );
    if report.changed_contracts.is_empty() {
        println!("no contract schema changes");
        return;
    }

    for contract in &report.changed_contracts {
        let from = contract.from_schema_id.as_deref().unwrap_or("-");
        let to = contract.to_schema_id.as_deref().unwrap_or("-");
        println!(
            "- {:?}: {} on {} ({} -> {})",
            contract.kind, contract.family, contract.topic, from, to
        );
    }
}
