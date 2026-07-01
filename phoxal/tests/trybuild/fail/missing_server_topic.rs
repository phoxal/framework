// Server topics are explicit in v1.
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Service)]
#[phoxal(id = "missing-server-topic", api = y2026_1)]
struct MissingServerTopic {}

#[phoxal::runtime]
impl MissingServerTopic {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[server]
    async fn get(
        &mut self,
        _request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        unimplemented!()
    }
}

fn main() {}
