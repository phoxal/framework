use serde::Serialize;

#[derive(Serialize)]
struct Generation<'a> {
    name: &'a str,
    channel: &'a str,
    extends: Option<&'a str>,
    contracts: Vec<Contract<'a>>,
}

#[derive(Serialize)]
struct Contract<'a> {
    family: &'a str,
    topic: &'a str,
    schema_id: &'a str,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let generations = phoxal_api::API_CONTRACT_MANIFEST
        .iter()
        .map(|generation| Generation {
            name: generation.name,
            channel: if generation.is_preview {
                "preview"
            } else {
                "stable"
            },
            extends: generation.extends,
            contracts: generation
                .contracts
                .iter()
                .map(|contract| Contract {
                    family: contract.family,
                    topic: contract.topic,
                    schema_id: contract.schema_id,
                })
                .collect(),
        })
        .collect::<Vec<_>>();

    serde_json::to_writer_pretty(std::io::stdout(), &generations)?;
    println!();
    Ok(())
}
