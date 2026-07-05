// Plan #15: a `Tool` is a thin raw-bus runner, not a typed-graph participant, so
// `#[server_snapshot]` (the concurrent query-server surface) is not allowed on it.
use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

struct MapState;

#[derive(phoxal::Tool)]
#[phoxal(id = "tool-forbids-server-snapshot", api = y2026_1)]
struct ToolForbidsServerSnapshot {}

#[phoxal::behavior]
impl ToolForbidsServerSnapshot {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[server_snapshot(topic = api::topic::new().map().submap())]
    async fn submap(
        _state: Snapshot<MapState>,
        _request: api::map::SubmapRequest,
    ) -> ServerResult<api::map::SubmapResponse> {
        Ok(api::map::SubmapResponse {
            width: 0,
            height: 0,
            resolution_m: 0.05,
            cells: Vec::new(),
        })
    }

    #[snapshot]
    fn snapshot(&self) -> MapState {
        MapState
    }
}

fn main() {}
