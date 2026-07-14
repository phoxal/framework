// #[phoxal(external)] on a Server field is a compile error for the same
// reason as a Publisher field: `serve` is a producer role (coherence-gate
// design §1), never required to have a counterpart.
use phoxal_api::v1 as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    #[phoxal(external)]
    get: Server<api::asset::GetRequest, api::asset::GetResponse>,
}

#[phoxal::service(id = "external-on-server")]
struct ExternalOnServer;

#[phoxal::behavior]
impl ExternalOnServer {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        unimplemented!()
    }
}

fn main() {}
