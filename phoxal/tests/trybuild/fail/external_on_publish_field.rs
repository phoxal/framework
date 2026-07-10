// #[phoxal(external)] on a Publisher field is a compile error: producer
// roles are never required to have counterparts (coherence-gate design §1),
// so the marker would be a permanent no-op there.
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    #[phoxal(external)]
    target: Publisher<api::drive::Target>,
}

#[phoxal::service(id = "external-on-publish")]
struct ExternalOnPublish;

#[phoxal::behavior]
impl ExternalOnPublish {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        unimplemented!()
    }
}

fn main() {}
