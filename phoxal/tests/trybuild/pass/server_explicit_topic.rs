// An explicit server topic expression can be any typed `Topic` value, as long as
// its key matches the request body's canonical topic. The supported way to build
// that value is the api-local topic builder - raw `Topic` constructors are sealed.
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

    #[server(topic = api::topic::new().asset().get())]
    async fn get(
        &mut self,
        _request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }
}

fn main() {}
