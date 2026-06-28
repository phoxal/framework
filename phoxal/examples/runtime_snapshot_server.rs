//! A scheduled runtime with committed snapshots and a concurrent read-only query.
//!
//! Run with `cargo run --example runtime_snapshot_server` or inspect metadata
//! with `cargo run --example runtime_snapshot_server emit-apis`.

use std::sync::Arc;

use phoxal::api::y2026_1 as api;
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

#[derive(phoxal::Runtime)]
#[phoxal(id = "snapshot-map", api = y2026_1)]
struct SnapshotMap {
    localize: Latest<api::localize::LocalizationState>,
    revision: Publisher<api::map::Revision>,
    grid: Arc<Grid>,
    rev: u64,
}

#[phoxal::runtime]
impl SnapshotMap {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            localize: ctx
                .subscribe(api::topic::new().localize().state())
                .latest()
                .await?,
            revision: ctx.publisher(api::topic::new().map().revision()).await?,
            grid: Arc::new(Grid::empty()),
            rev: 0,
        })
    }

    #[step(hz = 2)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        if self.localize.latest().is_some() {
            self.rev = self.rev.saturating_add(1);
        }
        self.revision
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

    #[server_snapshot(topic = api::topic::new().map().submap())]
    async fn submap(
        state: Snapshot<MapSnapshot>,
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
