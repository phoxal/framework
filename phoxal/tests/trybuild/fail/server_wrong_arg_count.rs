use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "server-extra-arg", api = y2026_1)]
struct ServerExtraArg {}

#[phoxal::runtime]
impl ServerExtraArg {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[server(topic = api::topic::internal::new().asset().get())]
    async fn get(
        &mut self,
        _request: api::asset::GetRequest,
        _extra: u32,
    ) -> ServerResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }
}

fn main() {}
