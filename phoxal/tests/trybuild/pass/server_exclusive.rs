// Exclusive query server fixture.
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "asset-pass", api = y2026_1)]
struct AssetPass {}

#[phoxal::runtime]
impl AssetPass {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[server(topic = api::topic::internal::new().asset().get())]
    async fn get(
        &mut self,
        _request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }
}

fn main() {}
