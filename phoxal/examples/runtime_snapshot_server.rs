//! A scheduled participant with committed snapshots and a concurrent read-only query.
//!
//! Run with `cargo run --example runtime_snapshot_server`.

use std::sync::Arc;

use phoxal::api;
use phoxal::prelude::*;

#[derive(Clone)]
struct Grid {
    width: u32,
    height: u32,
    resolution_m: f32,
    cells: Vec<u8>,
}

impl Grid {
    fn empty() -> Self {
        Self {
            width: 8,
            height: 8,
            resolution_m: 0.1,
            cells: vec![255; 64],
        }
    }

    fn response(&self) -> api::map::SubmapResponse {
        api::map::SubmapResponse {
            width: self.width,
            height: self.height,
            resolution_m: self.resolution_m,
            cells: self.cells.clone(),
        }
    }
}

struct MapSnapshot {
    grid: Arc<Grid>,
}

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    localize: Latest<api::localize::LocalizationState>,
    revision: Publisher<api::map::Revision>,
    get_asset: Server<api::asset::GetRequest, api::asset::GetResponse>,
    submap: Server<api::map::SubmapRequest, api::map::SubmapResponse>,
}

#[phoxal::service(id = "snapshot-map")]
struct SnapshotMap {
    grid: Arc<Grid>,
    rev: u64,
}

#[phoxal::behavior]
impl SnapshotMap {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability the owner
        // (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        Ok((
            Self {
                grid: Arc::new(Grid::empty()),
                rev: 0,
            },
            Self::Api {
                localize: ctx.latest(api::topic::new().localize().state()).await?,
                // This participant owns the map node's telemetry + queries it
                // serves below, so they go through the owner (`internal`)
                // builder; `localize/state` is consumed via the public builder.
                revision: ctx
                    .publisher(api::topic::internal::new(cap).map().revision())
                    .await?,
                get_asset: ctx.server(api::topic::new().asset().get()).await?,
                submap: ctx.server(api::topic::new().map().submap()).await?,
            },
        ))
    }

    #[step(hz = 2)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        if api.localize.latest().is_some() {
            self.rev = self.rev.saturating_add(1);
        }
        api.revision
            .publish_at(
                step.time(),
                api::map::Revision {
                    revision: self.rev,
                    resolution_m: self.grid.resolution_m,
                },
            )
            .await?;
        Ok(())
    }

    #[server(api = get_asset)]
    async fn get_asset(
        &mut self,
        _api: &mut Self::Api,
        request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        self.rev = self.rev.saturating_add(1);
        if request.path == "map.cells" {
            Ok(api::asset::GetResponse::Found {
                bytes: self.grid.cells.clone(),
            })
        } else {
            Ok(api::asset::GetResponse::Missing)
        }
    }

    #[server_snapshot(api = submap)]
    async fn submap(
        state: Snapshot<MapSnapshot>,
        _api: &Self::Api,
        _request: api::map::SubmapRequest,
    ) -> ServerResult<api::map::SubmapResponse> {
        Ok(state.grid.response())
    }

    #[snapshot]
    fn snapshot(&self) -> MapSnapshot {
        MapSnapshot {
            grid: Arc::clone(&self.grid),
        }
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<SnapshotMap>()
}
