// Plan #15: a `Tool` is a thin raw-bus runner, not a typed-graph participant, so
// `#[server]` (the exclusive query-server surface) is not allowed on it.
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Tool)]
#[phoxal(id = "tool-forbids-server", api = y2026_1)]
struct ToolForbidsServer {}

#[phoxal::behavior]
impl ToolForbidsServer {
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
