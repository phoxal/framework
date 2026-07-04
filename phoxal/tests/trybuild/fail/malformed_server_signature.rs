// Server handlers must return ServerResult<Resp>.
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Service)]
#[phoxal(id = "bad-server", api = y2026_1)]
struct BadServer {}

#[phoxal::behavior]
impl BadServer {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[server(topic = api::topic::new().asset().get())]
    async fn get(&mut self, _request: api::asset::GetRequest) -> Result<api::asset::GetResponse> {
        unimplemented!()
    }
}

fn main() {}
