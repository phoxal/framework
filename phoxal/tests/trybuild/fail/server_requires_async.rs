use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "server-sync", api = y2026_1)]
struct ServerSync {}

#[phoxal::runtime]
impl ServerSync {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[server(topic = api::topic::internal::new().asset().get())]
    fn get(&mut self, _request: api::asset::GetRequest) -> ServerResult<api::asset::GetResponse> {
        Ok(api::asset::GetResponse::Missing)
    }
}

fn main() {}
