// Canonical handle fields must name ContractBody types.
use phoxal::prelude::*;

struct NotABody;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    target: StatePublisher<NotABody>,
}

#[phoxal::service(id = "not-a-body")]
struct NotBodyRuntime;

#[phoxal::behavior]
impl NotBodyRuntime {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        unimplemented!()
    }
}

fn main() {}
