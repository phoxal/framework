use serde::Serialize;

#[derive(Serialize)]
struct Generation<'a> {
    name: &'a str,
    channel: &'a str,
    contracts: Vec<Contract<'a>>,
}

/// `family` is the version-qualified contract identity (e.g.
/// `"y2026_1::drive::Target"`); `topic` is its generation-qualified wire key
/// (e.g. `"y2026_1/drive/target"`). There is no `schema_id` (D1): the name
/// itself is the whole identity.
#[derive(Serialize)]
struct Contract<'a> {
    family: &'a str,
    topic: &'a str,
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
            contracts: generation
                .contracts
                .iter()
                .map(|contract| Contract {
                    family: contract.family,
                    topic: contract.topic,
                })
                .collect(),
        })
        .collect::<Vec<_>>();

    serde_json::to_writer_pretty(std::io::stdout(), &generations)?;
    println!();
    Ok(())
}
