//! `map` - localization-trace occupancy grid.
//!
//! A scheduled participant with a serialized query handler. It subscribes to
//! `localize/state`, publishes `map/revision` (the current revision and grid
//! resolution), and serves `map/submap` from the same state the step mutates.
//! Each step it marks the cell under the latest localization pose as free,
//! bumping the revision only when a cell actually changes.
//!
//! The grid is a fixed 64x64 window anchored at the map origin, it is built
//! from the localization trace alone (no range, depth or lidar observation
//! reaches it), and `map/submap` returns that whole window whatever bounds the
//! request asks for - so a consumer must treat the response's own
//! `width`/`height`/`resolution_m` as the extent it received, never the extent
//! it requested.

use phoxal::api;
use phoxal::bus::QueryFailure;
use phoxal::prelude::*;

const LOCALIZATION_STALE: std::time::Duration = std::time::Duration::from_secs(1);

pub(crate) struct Api {
    localize: Subscriber<api::localize::LocalizationState>,
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

    /// The whole grid, whatever window `request` asks for.
    ///
    /// The requested bounds are not honoured: the response describes its own
    /// extent, so a caller that reads `width`/`height`/`resolution_m` off the
    /// response is correct, and one that assumes it received the window it
    /// asked for reads cells from the wrong place.
    fn submap(&self, _request: &api::map::SubmapRequest) -> api::map::SubmapResponse {
        api::map::SubmapResponse {
            width: self.width,
            height: self.height,
            resolution_m: self.resolution_m,
            cells: self.cells.clone(),
        }
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
        ctx.query(api::topic::owner().map().submap(), Self::submap)
            .await?;
        Ok((
            MapState {
                grid: Grid::empty(64, 64, 0.05),
                rev: 0,
                has_localization: false,
                last_localization: None,
            },
            Api {
                localize: ctx
                    .subscriber(api::topic::client().localize().state(), 32)
                    .await?,
                // Map OWNS the `map` node (its revision telemetry and the
                // `map/submap` query it serves below) -> owner builder;
                // `localize/state` is consumed via the client builder.
                revision: ctx
                    .state_publisher(api::topic::owner().map().revision())
                    .await?,
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
        while let Some(observed) = api.localize.try_recv() {
            if let Some(at) = observed.metadata.produced_exactly_at() {
                state.last_localization = Some(Timed::new(observed.body, at));
            }
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
    async fn submap(
        &self,
        _api: &Api,
        request: api::map::SubmapRequest,
        state: &mut MapState,
    ) -> QueryResult<api::map::SubmapResponse> {
        if !state.has_localization {
            return Err(QueryFailure::unavailable(
                "map has no localization-backed revision yet",
            ));
        }
        Ok(state.grid.submap(&request))
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
