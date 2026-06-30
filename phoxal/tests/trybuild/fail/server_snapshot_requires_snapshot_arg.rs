use phoxal_api::y2026_1 as api;
use phoxal::prelude::*;

struct MapState;

#[derive(phoxal::Runtime)]
#[phoxal(id = "snapshot-wrong-state-arg", api = y2026_1)]
struct SnapshotWrongStateArg {}

#[phoxal::runtime]
impl SnapshotWrongStateArg {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {})
    }

    #[server_snapshot(topic = api::topic::new().map().submap())]
    async fn submap(
        _state: MapState,
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
