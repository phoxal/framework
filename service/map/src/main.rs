//! `map` - localization-trace occupancy-grid placeholder.
//!
//! A scheduled participant with a concurrent snapshot server. It subscribes to
//! `localize/state`, publishes `map/revision` (the current revision and grid
//! resolution), and serves `map/submap` from a committed grid snapshot.
//! It uses the concurrent snapshot-server pattern: `#[step]` updates the
//! copy-on-write grid, the runner commits `#[snapshot]` state, and
//! `#[server_snapshot]` serves `map/submap` concurrently from that snapshot
//! without blocking the step loop.
//! Each step it marks the cell under the latest localization pose as free,
//! bumping the revision only when a cell actually changes.
//! This is a placeholder: it does not integrate range/depth/lidar observations
//! yet, the grid is a fixed 64x64 window, and `map/submap` ignores the request
//! bounds and always returns the whole grid.

use std::sync::Arc;

use anyhow::Result;
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

#[derive(phoxal::Service)]
#[phoxal(id = "map", api = y2026_1)]
struct Map {
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
            cells: vec![255; (width * height) as usize],
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

#[phoxal::behavior]
impl Map {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        Ok(Self {
            localize: ctx
                .subscribe(api::topic::new().localize().state())
                .latest()
                .await?,
            // Map OWNS the `map` node (its revision telemetry and the `map/submap`
            // query it serves below) -> owner (`internal`) builder; `localize/state`
            // is CONSUMED via the public builder.
            revision: ctx
                .publisher(api::topic::internal::new(cap).map().revision())
                .await?,
            grid: Arc::new(Grid::empty(64, 64, 0.05)),
            rev: 0,
        })
    }

    #[step(hz = 5)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        if let Some(loc) = self.localize.latest() {
            // Copy-on-write before mutating, so committed snapshots stay stable.
            let grid = Arc::make_mut(&mut self.grid);
            if mark_free(grid, loc.x_m, loc.y_m) {
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

    // Concurrent read against the committed snapshot: does not block the step loop.
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

fn mark_free(grid: &mut Grid, x_m: f64, y_m: f64) -> bool {
    // Floor (not truncate-toward-zero) so a coordinate just below the origin maps
    // to a negative cell and is rejected, rather than folding onto cell 0.
    let x = (x_m / f64::from(grid.resolution_m)).floor();
    let y = (y_m / f64::from(grid.resolution_m)).floor();
    if !x.is_finite()
        || !y.is_finite()
        || x < 0.0
        || y < 0.0
        || x >= f64::from(grid.width)
        || y >= f64::from(grid.height)
    {
        return false;
    }

    let idx = (y as u32 * grid.width + x as u32) as usize;
    if grid.cells[idx] == 0 {
        return false;
    }

    grid.cells[idx] = 0;
    true
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Map>()
}

#[cfg(test)]
mod tests {
    use super::{Grid, Map, mark_free};
    use phoxal_api::ContractBody;
    use phoxal_api::y2026_1 as api;

    #[test]
    fn submap_returns_full_grid_window() {
        let grid = Grid::empty(4, 3, 0.25);
        let response = grid.submap(&api::map::SubmapRequest {
            min_x_m: 0.0,
            min_y_m: 0.0,
            max_x_m: 1.0,
            max_y_m: 1.0,
        });

        assert_eq!(response.width, 4);
        assert_eq!(response.height, 3);
        assert_eq!(response.resolution_m, 0.25);
        assert_eq!(response.cells.len(), 12);
        assert!(response.cells.iter().all(|cell| *cell == 255));
    }

    #[test]
    fn mark_free_sets_cell_and_reports_change() {
        let mut grid = Grid::empty(4, 3, 0.5);

        assert!(mark_free(&mut grid, 1.1, 0.6));
        assert_eq!(grid.cells[(grid.width + 2) as usize], 0);

        let after_first_mark = grid.cells.clone();
        assert!(!mark_free(&mut grid, 1.1, 0.6));
        assert_eq!(grid.cells, after_first_mark);

        assert!(!mark_free(&mut grid, 10.0, 10.0));
        assert_eq!(grid.cells, after_first_mark);

        // A coordinate just below the origin floors to a negative cell and is
        // rejected (it must not fold onto cell 0).
        assert!(!mark_free(&mut grid, -0.01, 0.0));
        assert!(!mark_free(&mut grid, 0.0, -0.01));
        assert_eq!(grid.cells, after_first_mark);
    }

    #[test]
    fn emit_apis_reports_map_contracts() {
        let json = phoxal::participant::emit_apis_json::<Map>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "map");

        let contracts = value["required_contracts"].as_array().unwrap();
        assert!(
            contracts
                .iter()
                .any(|c| c["topic"] == <api::map::Revision as ContractBody>::TOPIC)
        );
        assert!(
            contracts
                .iter()
                .any(|c| c["topic"] == <api::map::SubmapResponse as ContractBody>::TOPIC)
        );
        assert!(
            contracts.iter().any(|c| {
                c["topic"] == <api::localize::LocalizationState as ContractBody>::TOPIC
            })
        );
    }
}
