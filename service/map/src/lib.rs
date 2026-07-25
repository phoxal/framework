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

const LOCALIZATION_STALE_NS: u64 = 1_000_000_000;

#[derive(phoxal::Api)]
pub struct Api {
    localize: Subscriber<api::localize::LocalizationState>,
    revision: Publisher<api::map::Revision>,
    submap: Server<api::map::SubmapRequest, api::map::SubmapResponse>,
}

#[phoxal::service(id = "map", config = ())]
pub struct Map {
    grid: Arc<Grid>,
    rev: u64,
    has_localization: bool,
    last_localization: Option<(api::localize::LocalizationState, LogicalTime)>,
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
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        Ok((
            Self {
                grid: Arc::new(Grid::empty(64, 64, 0.05)),
                rev: 0,
                has_localization: false,
                last_localization: None,
            },
            Self::Api {
                localize: ctx
                    .subscriber(api::topic::new().localize().state(), 32)
                    .await?,
                // Map OWNS the `map` node (its revision telemetry and the
                // `map/submap` query it serves below) -> owner (`internal`)
                // builder; `localize/state` is CONSUMED via the public builder.
                revision: ctx
                    .publisher(api::topic::internal::new(cap).map().revision())
                    .await?,
                submap: ctx.server(api::topic::new().map().submap()).await?,
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
        while let Some(received) = api.localize.try_recv() {
            self.last_localization = Some((
                received.body,
                LogicalTime::new(received.metadata.epoch, received.metadata.produced_at_ns),
            ));
        }

        if !localization_is_usable(self.last_localization.as_ref(), step.time())? {
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
    sample: Option<&(api::localize::LocalizationState, LogicalTime)>,
    now: LogicalTime,
) -> Result<bool> {
    match sample {
        None => Ok(false),
        Some((_, at)) if at.epoch() != now.epoch() => Ok(false),
        Some((_, at)) if at.time_ns() > now.time_ns() => Ok(false),
        Some((_, at)) if now.time_ns().saturating_sub(at.time_ns()) > LOCALIZATION_STALE_NS => {
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
    use super::{Grid, LOCALIZATION_STALE_NS, Map, localization_is_usable, mark_free};
    use phoxal::api;
    use phoxal::bus::ContractBody;
    use phoxal::bus::LogicalTime;
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};

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
    fn localization_gate_rejects_unavailable_stale_future_epoch_and_invalid_samples() {
        let sample = |x_m, epoch, at| {
            (
                api::localize::LocalizationState {
                    x_m,
                    y_m: 0.0,
                    yaw_rad: 0.0,
                    confidence: 1.0,
                },
                LogicalTime::new(epoch, at),
            )
        };
        let now = LogicalTime::new(2, LOCALIZATION_STALE_NS + 10);
        assert!(!localization_is_usable(None, now).unwrap());
        assert!(localization_is_usable(Some(&sample(0.0, 2, 10)), now).unwrap());
        assert!(!localization_is_usable(Some(&sample(0.0, 1, now.time_ns())), now).unwrap());
        assert!(!localization_is_usable(Some(&sample(0.0, 2, now.time_ns() + 1)), now).unwrap());
        assert!(!localization_is_usable(Some(&sample(0.0, 2, 9)), now).unwrap());
        assert!(localization_is_usable(Some(&sample(f64::NAN, 2, now.time_ns())), now).is_err());
    }

    #[test]
    fn api_declares_the_map_contracts() {
        assert_eq!(<Map as Participant>::ID, "map");

        let contracts = <<Map as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert_contract::<api::localize::LocalizationState>(contracts, ContractRole::Subscribe);
        assert_contract::<api::map::Revision>(contracts, ContractRole::Publish);
        assert_contract::<api::map::SubmapRequest>(contracts, ContractRole::Serve);
        assert_contract::<api::map::SubmapResponse>(contracts, ContractRole::Serve);
    }

    fn assert_contract<B>(contracts: &[phoxal::participant::ApiContractUse], role: ContractRole)
    where
        B: ContractBody,
    {
        assert!(
            contracts
                .iter()
                .any(|c| c.topic == B::TOPIC && c.role == role),
            "expected a {role:?} contract for {} in {contracts:?}",
            B::TOPIC
        );
    }
}
