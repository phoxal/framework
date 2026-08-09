//! A scheduled participant with two queries sharing the same serialized state.
//!
//! Run with `cargo run --example runtime_serialized_queries`.

use phoxal::api;
use phoxal::bus::QueryFailure;
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

    fn response(
        &self,
        request: &api::map::SubmapRequest,
        revision: u64,
    ) -> QueryResult<api::map::SubmapResponse> {
        let requested = bounds_from_request(request)?;
        let map_max_x = f64::from(self.width) * f64::from(self.resolution_m);
        let map_max_y = f64::from(self.height) * f64::from(self.resolution_m);
        let min_x = requested.min_x_m.max(0.0);
        let min_y = requested.min_y_m.max(0.0);
        let max_x = requested.max_x_m.min(map_max_x);
        let max_y = requested.max_y_m.min(map_max_y);
        if !(min_x < max_x && min_y < max_y) {
            return Ok(api::map::SubmapResponse::OutOfBounds {
                requested,
                frame_id: "map".to_string(),
                revision,
            });
        }

        let resolution = f64::from(self.resolution_m);
        let start_x = cell_start(min_x, resolution, self.width);
        let start_y = cell_start(min_y, resolution, self.height);
        let end_x = cell_end(max_x, resolution, self.width);
        let end_y = cell_end(max_y, resolution, self.height);
        if start_x >= end_x || start_y >= end_y {
            return Ok(api::map::SubmapResponse::OutOfBounds {
                requested,
                frame_id: "map".to_string(),
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
                    .map(|cell| match cell {
                        0..=20 => api::map::Occupancy::Free,
                        255 => api::map::Occupancy::Unknown,
                        _ => api::map::Occupancy::Occupied,
                    }),
            );
        }
        let covered = api::map::Bounds {
            min_x_m: f64::from(start_x) * resolution,
            min_y_m: f64::from(start_y) * resolution,
            max_x_m: f64::from(end_x) * resolution,
            max_y_m: f64::from(end_y) * resolution,
        };
        let window = api::map::GridWindow {
            frame_id: "map".to_string(),
            origin_pose: api::map::Pose {
                x_m: covered.min_x_m,
                y_m: covered.min_y_m,
                yaw_rad: 0.0,
            },
            cell_origin: api::map::Point {
                x_m: covered.min_x_m,
                y_m: covered.min_y_m,
            },
            width,
            height,
            resolution_m: self.resolution_m,
            cells,
            revision,
            requested: requested.clone(),
            covered: covered.clone(),
        };
        Ok(if bounds_equal(&requested, &covered) {
            api::map::SubmapResponse::Window(window)
        } else {
            api::map::SubmapResponse::Partial { window }
        })
    }
}

struct Api {
    localize: StateView<api::localize::LocalizationState>,
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
        )?;
        ctx.query(api::topic::owner().map().submap(), Self::submap)?;
        Ok((
            MapState {
                grid: Grid::empty(),
                rev: 0,
            },
            Api {
                localize: ctx
                    .state_view(api::topic::client().localize().state())
                    .await?,
                revision: ctx.state_publisher(api::topic::owner().map().revision())?,
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
    fn get_asset(
        &self,
        _api: &Api,
        _query: QueryContext,
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

    fn submap(
        &self,
        _api: &Api,
        _query: QueryContext,
        request: api::map::SubmapRequest,
        state: &mut MapState,
    ) -> QueryResult<api::map::SubmapResponse> {
        state.grid.response(&request, state.rev)
    }
}

fn bounds_from_request(request: &api::map::SubmapRequest) -> QueryResult<api::map::Bounds> {
    if ![
        request.min_x_m,
        request.min_y_m,
        request.max_x_m,
        request.max_y_m,
    ]
    .into_iter()
    .all(f64::is_finite)
        || request.min_x_m >= request.max_x_m
        || request.min_y_m >= request.max_y_m
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

fn cell_start(min: f64, resolution: f64, dimension: u32) -> u32 {
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

fn bounds_equal(left: &api::map::Bounds, right: &api::map::Bounds) -> bool {
    let epsilon = 1.0e-6;
    (left.min_x_m - right.min_x_m).abs() <= epsilon
        && (left.min_y_m - right.min_y_m).abs() <= epsilon
        && (left.max_x_m - right.max_x_m).abs() <= epsilon
        && (left.max_y_m - right.max_y_m).abs() <= epsilon
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<SerializedMap>()
}
