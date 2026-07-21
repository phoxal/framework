use serde::Serialize;

#[derive(Serialize)]
struct Version<'a> {
    name: &'a str,
    channel: &'a str,
    contracts: Vec<Contract<'a>>,
}

/// `family` is the version-qualified contract identity (e.g.
/// `"v0.1::drive::Target"`); `topic` is its version-qualified wire key
/// (e.g. `"v0.1/drive/target"`). There is no `schema_id` (D1): the name
/// itself is the whole identity.
#[derive(Serialize)]
struct Contract<'a> {
    family: &'a str,
    topic: &'a str,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let versions = phoxal_api::API_CONTRACT_MANIFEST
        .iter()
        .map(|version| Version {
            name: version.name,
            channel: "revision",
            contracts: version
                .contracts
                .iter()
                .map(|contract| Contract {
                    family: contract.family,
                    topic: contract.topic,
                })
                .collect(),
        })
        .collect::<Vec<_>>();

    serde_json::to_writer_pretty(std::io::stdout(), &versions)?;
    println!();
    Ok(())
}
