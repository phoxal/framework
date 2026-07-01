use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Service)]
#[phoxal(id = "server-extra-arg", api = y2026_1)]
struct ServerExtraArg {}

#[phoxal::behavior]
impl ServerExtraArg {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[server(topic = api::topic::new().asset().get())]
    async fn get(
        &mut self,
        _request: api::asset::GetRequest,
        _extra: u32,
    ) -> ServerResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }
}

fn main() {}
