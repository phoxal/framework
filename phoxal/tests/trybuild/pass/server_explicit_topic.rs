// An explicit server topic expression can be a typed Topic expression, as long
// as its key matches the request body's canonical topic.
use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "asset-explicit-topic", api = y2026_1)]
struct AssetExplicitTopic {}

#[phoxal::runtime]
impl AssetExplicitTopic {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[server(topic = phoxal::bus::Topic::<phoxal::bus::Query<api::asset::GetRequest, api::asset::GetResponse>>::new_static("asset/get"))]
    async fn get(
        &mut self,
        _request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }
}

fn main() {}
