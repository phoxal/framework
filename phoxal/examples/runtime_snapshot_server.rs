//! A scheduled runtime with a concurrent, read-only `#[server_snapshot]` backed
//! by a committed `#[snapshot]`.
//!
//! `#[step]` mutates state behind an `Arc` (copy-on-write); the runner commits a
//! `Snapshot<MapState>` after setup and after each step, and `submap` reads it
//! concurrently without blocking the step loop (D16).

use std::sync::Arc;

use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "occupancy-map", api = y2026_1)]
struct OccupancyMap {
    localize: Latest<api::localize::LocalizationState>,
    revision: Publisher<api::map::Revision>,
    grid: Arc<Grid>,
    rev: u64,
}

#[derive(Clone)]
struct Grid {
    width: u32,
    height: u32,
    resolution_m: f32,
    cells: Vec<u8>,
}

impl Grid {
    fn empty(width: u32, height: u32, resolution_m: f32) -> Self {
        Grid {
            width,
            height,
            resolution_m,
            cells: vec![255; (width * height) as usize], // 255 = unknown
        }
    }

    fn submap(&self, _request: &api::map::SubmapRequest) -> api::map::SubmapResponse {
        api::map::SubmapResponse {
            width: self.width,
            height: self.height,
            resolution_m: self.resolution_m,
            cells: self.cells.clone(),
        }
    }
}

// The committed snapshot type: cheap to clone (an `Arc` over the grid).
struct MapState {
    grid: Arc<Grid>,
}

#[phoxal::runtime]
impl OccupancyMap {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            localize: ctx
                .subscribe(api::topic::new().localize().state())
                .latest()
                .await?,
            revision: ctx.publisher(api::topic::new().map().revision()).await?,
            grid: Arc::new(Grid::empty(64, 64, 0.05)),
            rev: 0,
        })
    }

    #[step(hz = 5)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        if let Some(loc) = self.localize.latest() {
            // Copy-on-write before mutating, so committed snapshots stay stable.
            let grid = Arc::make_mut(&mut self.grid);
            let x = (loc.x_m / grid.resolution_m as f64) as i64;
            let y = (loc.y_m / grid.resolution_m as f64) as i64;
            if x >= 0 && y >= 0 && (x as u32) < grid.width && (y as u32) < grid.height {
                let idx = (y as u32 * grid.width + x as u32) as usize;
                grid.cells[idx] = 0; // free
                self.rev += 1;
            }
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

    // Concurrent read against the committed snapshot — does not block the step loop.
    #[server_snapshot(topic = api::topic::new().map().submap())]
    async fn submap(
        state: Snapshot<MapState>,
        request: api::map::SubmapRequest,
    ) -> ServerResult<api::map::SubmapResponse> {
        Ok(state.grid.submap(&request))
    }

    #[snapshot]
    fn snapshot(&self) -> MapState {
        MapState {
            grid: Arc::clone(&self.grid),
        }
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<OccupancyMap>()
}
