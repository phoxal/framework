// Two fields naming the same contract in the same role must agree on
// #[phoxal(external)] (coherence-gate design §1) - the derive rejects a
// marked/unmarked mix rather than guessing which field's intent wins.
use phoxal_api::v1 as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    #[phoxal(external)]
    target_a: Latest<api::drive::Target>,
    target_b: Subscriber<api::drive::Target>,
}

#[phoxal::service(id = "external-marker-disagreement")]
struct ExternalMarkerDisagreement;

#[phoxal::behavior]
impl ExternalMarkerDisagreement {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        unimplemented!()
    }
}

fn main() {}
