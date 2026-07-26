// Snapshot query server fixture.
use phoxal::api as api;
use phoxal::prelude::*;

struct MapState;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    submap: Server<api::map::SubmapRequest, api::map::SubmapResponse>,
}

#[phoxal::service(id = "map-pass")]
struct MapPass;

#[phoxal::behavior]
impl MapPass {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self,
            Self::Api {
                submap: ctx.server(api::topic::client().map().submap()).await?,
            },
        ))
    }

    #[server_snapshot(api = submap)]
    async fn submap(
        _state: Snapshot<MapState>,
        _api: &Self::Api,
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
