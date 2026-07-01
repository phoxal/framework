use phoxal_api::y2026_1 as api;

#[derive(phoxal::Service)]
#[phoxal(
    id = "bad-contract-tokens",
    api = y2026_1,
    contracts(publishes(api::drive::Target trailing))
)]
struct BadContractTokens {}

fn main() {}
