use phoxal::api::y2026_1 as api;

#[derive(phoxal::Runtime)]
#[phoxal(
    id = "bad-contract-tokens",
    api = y2026_1,
    contracts(publishes(api::drive::Target trailing))
)]
struct BadContractTokens {}

fn main() {}
