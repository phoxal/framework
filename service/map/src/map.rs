//! `map` - localization-trace occupancy grid.
//!
//! A scheduled participant with a serialized query handler. It subscribes to
//! `localize/state`, publishes `map/revision` (the current revision and grid
//! resolution), and serves a bounded window from the same state the step
//! mutates. Each step it marks the cell under the latest localization pose as
//! free, bumping the revision only when a cell actually changes.
//!
//! Queries are clipped to the fixed 64x64 map and report the exact requested
//! and covered bounds in the response. A request that does not intersect the
//! map receives an explicit `OutOfBounds` outcome; no query is silently
//! replaced with the whole grid.

use phoxal::api;
use phoxal::bus::QueryFailure;
use phoxal::prelude::*;

const LOCALIZATION_STALE: std::time::Duration = std::time::Duration::from_secs(1);

pub(crate) struct Api {
    localize: StateView<api::localize::LocalizationState>,
    revision: StatePublisher<api::map::Revision>,
}

pub(crate) struct MapState {
    grid: Grid,
    rev: u64,
    has_localization: bool,
    last_localization: Option<Timed<api::localize::LocalizationState>>,
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

    /// Return the requested window, clipping only at the map boundary.
    fn submap(
        &self,
        request: &api::map::SubmapRequest,
        revision: u64,
    ) -> QueryResult<api::map::SubmapResponse> {
        let requested = bounds_from_request(request)?;
        let map_bounds = api::map::Bounds {
            min_x_m: 0.0,
            min_y_m: 0.0,
            max_x_m: f64::from(self.width) * f64::from(self.resolution_m),
            max_y_m: f64::from(self.height) * f64::from(self.resolution_m),
        };
        let min_x = requested.min_x_m.max(map_bounds.min_x_m);
        let min_y = requested.min_y_m.max(map_bounds.min_y_m);
        let max_x = requested.max_x_m.min(map_bounds.max_x_m);
        let max_y = requested.max_y_m.min(map_bounds.max_y_m);
        if !(min_x < max_x && min_y < max_y) {
            return Ok(api::map::SubmapResponse::OutOfBounds {
                requested,
                frame_id: MAP_FRAME.to_string(),
                revision,
            });
        }

        let resolution = f64::from(self.resolution_m);
        // Return only complete cells whose physical extent is contained in the
        // requested bounds. This keeps `covered` truthful for unaligned
        // requests instead of returning a cell that extends beyond the query.
        // A query narrower than one cell has an explicit typed outcome below.
        let start_x = cell_start(min_x, resolution, self.width);
        let start_y = cell_start(min_y, resolution, self.height);
        let end_x = cell_end(max_x, resolution, self.width);
        let end_y = cell_end(max_y, resolution, self.height);
        if start_x >= end_x || start_y >= end_y {
            return Ok(api::map::SubmapResponse::OutOfBounds {
                requested,
                frame_id: MAP_FRAME.to_string(),
                revision,
            });
        }

        let width = end_x - start_x;
        let height = end_y - start_y;
        let mut cells = Vec::with_capacity((width as usize) * (height as usize));
        for y in start_y..end_y {
            let row_start = (y * self.width + start_x) as usize;
            let row_end = row_start + width as usize;
            cells.extend(
                self.cells[row_start..row_end]
                    .iter()
                    .copied()
                    .map(occupancy),
            );
        }
        let covered = api::map::Bounds {
            min_x_m: f64::from(start_x) * resolution,
            min_y_m: f64::from(start_y) * resolution,
            max_x_m: f64::from(end_x) * resolution,
            max_y_m: f64::from(end_y) * resolution,
        };
        let window = api::map::GridWindow {
            frame_id: MAP_FRAME.to_string(),
            origin_pose: api::map::Pose {
                x_m: covered.min_x_m,
                y_m: covered.min_y_m,
                yaw_rad: 0.0,
            },
            cell_origin: api::map::Point {
                x_m: covered.min_x_m,
                y_m: covered.min_y_m,
            },
            resolution_m: self.resolution_m,
            width,
            height,
            cells,
            revision,
            requested: requested.clone(),
            covered: covered.clone(),
        };
        let complete = bounds_equal(&requested, &covered);
        Ok(if complete {
            api::map::SubmapResponse::Window(window)
        } else {
            api::map::SubmapResponse::Partial { window }
        })
    }

    /// Mark the cell containing `(x_m, y_m)` free, reporting whether that
    /// changed the grid.
    fn mark_free(&mut self, x_m: f64, y_m: f64) -> bool {
        // Floor (not truncate-toward-zero) so a coordinate just below the origin maps
        // to a negative cell and is rejected, rather than folding onto cell 0.
        let x = (x_m / f64::from(self.resolution_m)).floor();
        let y = (y_m / f64::from(self.resolution_m)).floor();
        if !x.is_finite()
            || !y.is_finite()
            || x < 0.0
            || y < 0.0
            || x >= f64::from(self.width)
            || y >= f64::from(self.height)
        {
            return false;
        }

        let idx = (y as u32 * self.width + x as u32) as usize;
        if self.cells[idx] == 0 {
            return false;
        }

        self.cells[idx] = 0;
        true
    }
}

#[phoxal::service(state = MapState, api = Api)]
pub(crate) struct Map;

impl Participant for Map {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        ctx.query(api::topic::owner().map().submap(), Self::submap)?;
        Ok((
            MapState {
                grid: Grid::empty(64, 64, 0.05),
                rev: 0,
                has_localization: false,
                last_localization: None,
            },
            Api {
                localize: ctx
                    .state_view(api::topic::client().localize().state())
                    .await?,
                // Map OWNS the `map` node (its revision telemetry and the
                // `map/submap` query it serves below) -> owner builder;
                // `localize/state` is consumed via the client builder.
                revision: ctx.state_publisher(api::topic::owner().map().revision())?,
            },
        ))
    }

    fn reset(&self, _ctx: ResetContext, _api: &Self::Api, state: &mut Self::State) -> Result<()> {
        let (width, height, resolution_m) =
            (state.grid.width, state.grid.height, state.grid.resolution_m);
        state.grid = Grid::empty(width, height, resolution_m);
        state.rev = 0;
        state.has_localization = false;
        state.last_localization = None;
        Ok(())
    }

    #[phoxal::step(hz = 5)]
    fn step(&self, api: &Self::Api, step: StepContext, state: &mut Self::State) -> Result<()> {
        if let Some(observed) = api.localize.observed()
            && let Some(at) = observed.metadata.produced_exactly_at()
        {
            state.last_localization = Some(Timed::new(observed.body.clone(), at));
        }

        // Only a real, finite, fresh pose may write the grid; anything else
        // leaves the revision where it is and marks the map unbacked. The gate
        // hands the pose it proved straight to the writer.
        let Some(localization) = state.last_localization.as_ref() else {
            state.has_localization = false;
            return Ok(());
        };
        // A sample from a replaced world, from this step's future, or older than
        // the window is not usable; `fresh_within` answers all three and fails
        // closed across timelines.
        if !localization.fresh_within(step.now(), LOCALIZATION_STALE) {
            state.has_localization = false;
            return Ok(());
        }
        if !localization_is_finite(&localization.body) {
            anyhow::bail!("localization sample contains a non-finite value");
        }

        state.has_localization = true;
        let (x_m, y_m) = (localization.body.x_m, localization.body.y_m);
        if state.grid.mark_free(x_m, y_m) {
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

impl Map {
    fn submap(
        &self,
        _api: &Api,
        _query: QueryContext,
        request: api::map::SubmapRequest,
        state: &mut MapState,
    ) -> QueryResult<api::map::SubmapResponse> {
        if !state.has_localization {
            return Err(QueryFailure::unavailable(
                "map has no localization-backed revision yet",
            ));
        }
        state.grid.submap(&request, state.rev)
    }
}

const MAP_FRAME: &str = "map";

fn bounds_from_request(request: &api::map::SubmapRequest) -> QueryResult<api::map::Bounds> {
    if !([
        request.min_x_m,
        request.min_y_m,
        request.max_x_m,
        request.max_y_m,
    ]
    .into_iter()
    .all(f64::is_finite)
        && request.min_x_m < request.max_x_m
        && request.min_y_m < request.max_y_m)
    {
        return Err(QueryFailure::invalid_argument(
            "map query bounds must be finite and have positive extent",
        ));
    }
    Ok(api::map::Bounds {
        min_x_m: request.min_x_m,
        min_y_m: request.min_y_m,
        max_x_m: request.max_x_m,
        max_y_m: request.max_y_m,
    })
}

fn bounds_equal(left: &api::map::Bounds, right: &api::map::Bounds) -> bool {
    let epsilon = 1.0e-6;
    (left.min_x_m - right.min_x_m).abs() <= epsilon
        && (left.min_y_m - right.min_y_m).abs() <= epsilon
        && (left.max_x_m - right.max_x_m).abs() <= epsilon
        && (left.max_y_m - right.max_y_m).abs() <= epsilon
}

fn cell_start(min: f64, resolution: f64, dimension: u32) -> u32 {
    // The small tolerance preserves an exact boundary after a representable
    // decimal such as 0.3/0.1 is rounded just below its mathematical value.
    let ratio = min / resolution;
    let nearest = ratio.round();
    let index = if (ratio - nearest).abs() <= 1.0e-9 {
        nearest
    } else {
        ratio.ceil()
    };
    index.clamp(0.0, f64::from(dimension)) as u32
}

fn cell_end(max: f64, resolution: f64, dimension: u32) -> u32 {
    let ratio = max / resolution;
    let nearest = ratio.round();
    let index = if (ratio - nearest).abs() <= 1.0e-9 {
        nearest
    } else {
        ratio.floor()
    };
    index.clamp(0.0, f64::from(dimension)) as u32
}

fn occupancy(cell: u8) -> api::map::Occupancy {
    match cell {
        0..=20 => api::map::Occupancy::Free,
        255 => api::map::Occupancy::Unknown,
        _ => api::map::Occupancy::Occupied,
    }
}

/// A non-finite pose would floor into a meaningless cell index, so it stops the
/// step instead of reaching the grid.
fn localization_is_finite(localization: &api::localize::LocalizationState) -> bool {
    localization.x_m.is_finite()
        && localization.y_m.is_finite()
        && localization.yaw_rad.is_finite()
        && localization.confidence.is_finite()
}

#[cfg(test)]
mod tests {
    use super::{Grid, localization_is_finite};
    use phoxal::api;

    fn localization(x_m: f64) -> api::localize::LocalizationState {
        api::localize::LocalizationState {
            x_m,
            y_m: 0.0,
            yaw_rad: 0.0,
            confidence: 1.0,
        }
    }

    #[test]
    fn submap_returns_full_grid_window() {
        let grid = Grid::empty(4, 3, 0.25);
        let response = grid
            .submap(
                &api::map::SubmapRequest {
                    min_x_m: 0.0,
                    min_y_m: 0.0,
                    max_x_m: 1.0,
                    max_y_m: 0.75,
                },
                7,
            )
            .expect("valid query");

        let api::map::SubmapResponse::Window(response) = response else {
            panic!("the request exactly covers the map");
        };
        assert_eq!(response.width, 4);
        assert_eq!(response.height, 3);
        assert_eq!(response.resolution_m, 0.25);
        assert_eq!(response.revision, 7);
        assert_eq!(response.cell_origin, api::map::Point { x_m: 0.0, y_m: 0.0 });
        assert_eq!(
            response.origin_pose,
            api::map::Pose {
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: 0.0
            }
        );
        assert_eq!(response.cells.len(), 12);
        assert!(
            response
                .cells
                .iter()
                .all(|cell| *cell == api::map::Occupancy::Unknown)
        );
    }

    #[test]
    fn submap_reports_clipping_and_origin_instead_of_ignoring_bounds() {
        let grid = Grid::empty(4, 3, 0.25);
        let response = grid
            .submap(
                &api::map::SubmapRequest {
                    min_x_m: -0.5,
                    min_y_m: 0.25,
                    max_x_m: 0.5,
                    max_y_m: 0.75,
                },
                9,
            )
            .expect("valid query");

        let api::map::SubmapResponse::Partial { window } = response else {
            panic!("the request must be reported as partial");
        };
        assert_eq!(
            window.cell_origin,
            api::map::Point {
                x_m: 0.0,
                y_m: 0.25
            }
        );
        assert_eq!(window.origin_pose.x_m, 0.0);
        assert_eq!(window.origin_pose.y_m, 0.25);
        assert_eq!(window.width, 2);
        assert_eq!(window.height, 2);
        assert_eq!(window.requested.min_x_m, -0.5);
        assert_eq!(window.covered.min_x_m, 0.0);
    }

    #[test]
    fn submap_keeps_unaligned_covered_extent_inside_the_request() {
        let grid = Grid::empty(4, 3, 0.25);
        let response = grid
            .submap(
                &api::map::SubmapRequest {
                    min_x_m: 0.1,
                    min_y_m: 0.1,
                    max_x_m: 0.9,
                    max_y_m: 0.9,
                },
                10,
            )
            .expect("valid query");

        let api::map::SubmapResponse::Partial { window } = response else {
            panic!("an unaligned request should report a partial window");
        };
        assert!(window.covered.min_x_m >= window.requested.min_x_m);
        assert!(window.covered.min_y_m >= window.requested.min_y_m);
        assert!(window.covered.max_x_m <= window.requested.max_x_m);
        assert!(window.covered.max_y_m <= window.requested.max_y_m);
        assert_eq!(window.width, 2);
        assert_eq!(window.height, 2);
    }

    #[test]
    fn submap_reports_out_of_bounds() {
        let grid = Grid::empty(4, 3, 0.25);
        let response = grid
            .submap(
                &api::map::SubmapRequest {
                    min_x_m: 2.0,
                    min_y_m: 0.0,
                    max_x_m: 3.0,
                    max_y_m: 1.0,
                },
                11,
            )
            .expect("finite query");

        assert!(matches!(
            response,
            api::map::SubmapResponse::OutOfBounds { revision: 11, .. }
        ));
    }

    #[test]
    fn mark_free_sets_cell_and_reports_change() {
        let mut grid = Grid::empty(4, 3, 0.5);

        assert!(grid.mark_free(1.1, 0.6));
        assert_eq!(grid.cells[(grid.width + 2) as usize], 0);

        let after_first_mark = grid.cells.clone();
        assert!(!grid.mark_free(1.1, 0.6));
        assert_eq!(grid.cells, after_first_mark);

        assert!(!grid.mark_free(10.0, 10.0));
        assert_eq!(grid.cells, after_first_mark);

        // A coordinate just below the origin floors to a negative cell and is
        // rejected (it must not fold onto cell 0).
        assert!(!grid.mark_free(-0.01, 0.0));
        assert!(!grid.mark_free(0.0, -0.01));
        assert_eq!(grid.cells, after_first_mark);
    }

    /// Every field is checked, so a non-finite value anywhere in the pose stops
    /// the step rather than reaching the grid.
    #[test]
    fn a_non_finite_field_anywhere_makes_the_pose_unusable() {
        assert!(localization_is_finite(&localization(0.0)));

        for broken in [
            api::localize::LocalizationState {
                x_m: f64::NAN,
                ..localization(0.0)
            },
            api::localize::LocalizationState {
                y_m: f64::INFINITY,
                ..localization(0.0)
            },
            api::localize::LocalizationState {
                yaw_rad: f64::NEG_INFINITY,
                ..localization(0.0)
            },
            api::localize::LocalizationState {
                confidence: f32::NAN,
                ..localization(0.0)
            },
        ] {
            assert!(
                !localization_is_finite(&broken),
                "{broken:?} must not be finite"
            );
        }
    }
}
