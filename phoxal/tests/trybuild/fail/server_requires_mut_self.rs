use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Service)]
#[phoxal(id = "server-shared-self", api = y2026_1)]
struct ServerSharedSelf {}

#[phoxal::runtime]
impl ServerSharedSelf {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[server(topic = api::topic::new().asset().get())]
    async fn get(
        &self,
        _request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }
}

fn main() {}
