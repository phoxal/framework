//! A scheduled participant with two queries sharing the same serialized state.
//!
//! Run with `cargo run --example runtime_serialized_queries`.

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

struct Api {
    localize: Latest<api::localize::LocalizationState>,
    revision: StatePublisher<api::map::Revision>,
}

struct MapState {
    grid: Grid,
    rev: u64,
}

#[phoxal::service(id = "serialized-map", state = MapState, api = Api)]
struct SerializedMap;

impl Participant for SerializedMap {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        ctx.query(
            api::topic::owner().supervisor().asset().get(),
            Self::get_asset,
        )
        .await?;
        ctx.query(api::topic::owner().map().submap(), Self::submap)
            .await?;
        Ok((
            MapState {
                grid: Grid::empty(),
                rev: 0,
            },
            Api {
                localize: ctx.latest(api::topic::client().localize().state()).await?,
                revision: ctx
                    .state_publisher(api::topic::owner().map().revision())
                    .await?,
            },
        ))
    }

    #[phoxal::step(hz = 2)]
    fn step(&self, api: &Self::Api, step: StepContext, state: &mut Self::State) -> Result<()> {
        if api.localize.latest().is_some() {
            state.rev = state.rev.saturating_add(1);
        }
        api.revision.publish(
            &step.token,
            api::map::Revision {
                revision: state.rev,
                resolution_m: state.grid.resolution_m,
            },
        )?;
        Ok(())
    }
}

impl SerializedMap {
    async fn get_asset(
        &self,
        _api: &Api,
        request: api::supervisor::asset::GetRequest,
        state: &mut MapState,
    ) -> QueryResult<api::supervisor::asset::GetResponse> {
        state.rev = state.rev.saturating_add(1);
        if request.path == "map.cells" {
            Ok(api::supervisor::asset::GetResponse::Found {
                bytes: state.grid.cells.clone(),
            })
        } else {
            Ok(api::supervisor::asset::GetResponse::Missing)
        }
    }

    async fn submap(
        &self,
        _api: &Api,
        _request: api::map::SubmapRequest,
        state: &mut MapState,
    ) -> QueryResult<api::map::SubmapResponse> {
        Ok(state.grid.response())
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<SerializedMap>()
}
