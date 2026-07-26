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
use phoxal::api;
use phoxal::bus::QueryFailure;
use phoxal::prelude::*;

const LOCALIZATION_STALE: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(phoxal::Api)]
pub struct Api {
    localize: Subscriber<api::localize::LocalizationState>,
    revision: StatePublisher<api::map::Revision>,
    submap: Server<api::map::SubmapRequest, api::map::SubmapResponse>,
}

#[phoxal::service(id = "map", config = ())]
pub struct Map {
    grid: Arc<Grid>,
    rev: u64,
    has_localization: bool,
    last_localization: Option<(api::localize::LocalizationState, RobotInstant)>,
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
pub struct MapState {
    grid: Arc<Grid>,
    ready: bool,
}

#[phoxal::behavior]
impl Map {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self {
                grid: Arc::new(Grid::empty(64, 64, 0.05)),
                rev: 0,
                has_localization: false,
                last_localization: None,
            },
            Self::Api {
                localize: ctx
                    .subscriber(api::topic::client().localize().state(), 32)
                    .await?,
                // Map OWNS the `map` node (its revision telemetry and the
                // `map/submap` query it serves below) -> owner
                // builder; `localize/state` is CONSUMED via the public builder.
                revision: ctx
                    .state_publisher(api::topic::owner().map().revision())
                    .await?,
                submap: ctx.server(api::topic::client().map().submap()).await?,
            },
        ))
    }

    #[reset]
    async fn reset(&mut self, _ctx: ResetContext) -> Result<()> {
        let (width, height, resolution_m) =
            (self.grid.width, self.grid.height, self.grid.resolution_m);
        self.grid = Arc::new(Grid::empty(width, height, resolution_m));
        self.rev = 0;
        self.has_localization = false;
        self.last_localization = None;
        Ok(())
    }

    #[step(hz = 5)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        while let Some(observed) = api.localize.try_recv() {
            if let Some(at) = observed.metadata.produced_exactly_at() {
                self.last_localization = Some((observed.body, at));
            }
        }

        if !localization_is_usable(self.last_localization.as_ref(), step.now())? {
            self.has_localization = false;
            return Ok(());
        }

        if let Some((loc, _)) = &self.last_localization {
            self.has_localization = true;
            // Copy-on-write before mutating, so committed snapshots stay stable.
            let grid = Arc::make_mut(&mut self.grid);
            if mark_free(grid, loc.x_m, loc.y_m) {
                self.rev += 1;
            }
        }

        api.revision.publish(
            step.token(),
            api::map::Revision {
                revision: self.rev,
                resolution_m: self.grid.resolution_m,
            },
        )?;
        Ok(())
    }

    // Concurrent read against the committed snapshot: does not block the step loop.
    // Reads only committed `Snapshot` state, never touches a `Subscriber` (the
    // `localize` field is a non-destructive `Latest`, safe even if read, but this
    // handler does not need it either).
    #[server_snapshot(api = submap)]
    async fn submap(
        state: Snapshot<MapState>,
        api: &Self::Api,
        request: api::map::SubmapRequest,
    ) -> ServerResult<api::map::SubmapResponse> {
        let _ = api;
        if !state.ready {
            return Err(QueryFailure::unavailable(
                "map has no localization-backed revision yet",
            ));
        }
        Ok(state.grid.submap(&request))
    }

    #[snapshot]
    fn snapshot(&self) -> MapState {
        MapState {
            grid: Arc::clone(&self.grid),
            ready: self.has_localization,
        }
    }
}

fn localization_is_usable(
    sample: Option<&(api::localize::LocalizationState, RobotInstant)>,
    now: RobotInstant,
) -> Result<bool> {
    match sample {
        None => Ok(false),
        // A sample from a replaced world, from this step's future, or older
        // than the window is not usable; the checked predicate answers all
        // three and can never silently pass across timelines.
        Some((_, at))
            if !TimeWindow::exact(*at)
                .possibly_fresh_within(now, LOCALIZATION_STALE)
                .unwrap_or(false) =>
        {
            Ok(false)
        }
        Some((loc, _))
            if !loc.x_m.is_finite()
                || !loc.y_m.is_finite()
                || !loc.yaw_rad.is_finite()
                || !loc.confidence.is_finite() =>
        {
            anyhow::bail!("localization sample contains a non-finite value")
        }
        Some(_) => Ok(true),
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

#[cfg(test)]
mod tests {
    use super::{Grid, LOCALIZATION_STALE, localization_is_usable, mark_free};
    use phoxal::api;
    use phoxal::bus::RobotInstant;

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
    fn localization_gate_rejects_unavailable_stale_cross_timeline_and_invalid_samples() {
        let sample = |x_m, timeline, at| {
            (
                api::localize::LocalizationState {
                    x_m,
                    y_m: 0.0,
                    yaw_rad: 0.0,
                    confidence: 1.0,
                },
                RobotInstant::new(timeline, at),
            )
        };
        let line = phoxal::bus::TimelineId::mint();
        let replaced = phoxal::bus::TimelineId::mint();
        let stale_ns = u64::try_from(LOCALIZATION_STALE.as_nanos()).unwrap();
        let now = RobotInstant::new(line, stale_ns + 10);

        assert!(!localization_is_usable(None, now).unwrap());
        assert!(localization_is_usable(Some(&sample(0.0, line, 10)), now).unwrap());
        assert!(
            !localization_is_usable(Some(&sample(0.0, replaced, now.ticks())), now).unwrap(),
            "a sample from a replaced world is incomparable, never usable"
        );
        assert!(
            !localization_is_usable(Some(&sample(0.0, line, now.ticks() + 1)), now).unwrap(),
            "a sample from this step's future is not usable"
        );
        assert!(
            !localization_is_usable(Some(&sample(0.0, line, 9)), now).unwrap(),
            "a sample older than the window is not usable"
        );
        assert!(localization_is_usable(Some(&sample(f64::NAN, line, now.ticks())), now).is_err());
    }
}
