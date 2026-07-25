use phoxal::api;
use phoxal::prelude::*;

#[derive(phoxal::Api)]
struct Api {
    #[phoxal(epoch_agnostic)]
    target: Publisher<api::drive::Target>,
}

#[phoxal::service(id = "epoch-agnostic-publisher", config = (), api = Api)]
struct InvalidApi;

#[phoxal::behavior]
impl InvalidApi {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        unimplemented!()
    }
}

fn main() {}
