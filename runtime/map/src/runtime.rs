use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use nalgebra::{Quaternion, UnitQuaternion};
use phoxal::api::component::v1::capability::range;
use phoxal::api::frame::v1::FrameId;
use phoxal::api::localize::v1::{
    Keyframe, LocalizationRevision, LocalizationRevisionId, LocalizationState, PoseEstimate,
    keyframe as localize_keyframe, revision as localize_revision, state as localize_state,
};
use phoxal::api::map::v1::{
    EsdfTile, EsdfTileRequest, EsdfTileResponse, GlobalGrid, GlobalGridRequest, GlobalGridResponse,
    Grid, LocalCost, LocalGrid, LocalGridRequest, LocalGridResponse, MapRevision, MapRevisionId,
    MapTileResponse, RegionSummary, Snapshot, SnapshotRequest, SnapshotResponse, SubmapRequest,
    SubmapResponse, Summary, Traversability, TraversabilityCell, TraversabilityStatus,
    TraversabilitySummary, TraversabilityTile, TraversabilityTileRequest,
    TraversabilityTileResponse, local_cost, query::esdf_tile, query::global_grid,
    query::local_grid, query::snapshot, query::submap, query::traversability_tile, revision,
    summary, traversability, traversability_summary,
};
use phoxal::bus::pubsub::Stamped;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::runtime::clock::Step;
use phoxal::runtime::runtime::{Io, Publisher, Runtime, RuntimeInputs};
use phoxal::runtime::{EmptyArgs, QueryOptions, ReadCell, RobotRuntimeArgs};
use phoxal::spatial::ray::sample_range_rays;
use phoxal::spatial::sensor::{
    ResolvedSensorKind, ResolvedSensorPose, resolve_sensor_poses_in_frame,
};
use tracing::info;

use crate::core::body_envelope;
use crate::core::occupancy::OccupancyGrid;
use crate::core::revisions::{RetainedRevision, RevisionLookup, RevisionStore};
use crate::core::submaps::SubmapStore;

use crate::selector;

const CLOCK_PERIOD: Duration = Duration::from_millis(100);
const PLANAR_FRAME_ID: &str = "map";
const BASE_LINK_ID: &str = "base_link";
const BASE_FOOTPRINT_ID: &str = "base_footprint";

#[derive(Clone)]
pub struct Config {
    planar_frame_id: FrameId,
    clock_period: Duration,
    mapping_range_inputs: Vec<CapabilityRef>,
    mapping_sensor_poses: BTreeMap<CapabilityRef, ResolvedSensorPose>,
    body_radius_m: f64,
}

impl Config {
    pub fn from_args(args: &RobotRuntimeArgs) -> Result<Self> {
        let robot = args.robot()?;
        let structure = args.structure()?;
        let mapping_range_inputs = selector::detect_mapping_range_inputs(&robot);
        let mapping_sensor_poses = if mapping_range_inputs.is_empty() {
            BTreeMap::new()
        } else {
            let resolved_sensor_poses = resolve_sensor_poses_in_frame(
                &robot.model,
                &robot.components,
                &structure,
                &mapping_range_inputs,
                BASE_FOOTPRINT_ID,
            )?;
            mapping_range_inputs
                .iter()
                .cloned()
                .zip(resolved_sensor_poses)
                .collect()
        };
        Ok(Self {
            planar_frame_id: FrameId::new(PLANAR_FRAME_ID),
            clock_period: CLOCK_PERIOD,
            mapping_range_inputs,
            mapping_sensor_poses,
            body_radius_m: body_envelope::body_radius_from_structure(&structure, BASE_LINK_ID)?,
        })
    }

    pub const fn clock_period(&self) -> Duration {
        self.clock_period
    }
}

pub enum Input {
    LocalizationState(Stamped<LocalizationState>),
    LocalizationRevision(Stamped<LocalizationRevision>),
    Keyframe(Stamped<Keyframe>),
    RangeSample(CapabilityRef, Stamped<range::Sample>),
}

pub struct MapView {
    planar_frame_id: FrameId,
    body_radius_m: f64,
    store: RevisionStore,
    submaps: SubmapStore,
    occupancy: Option<Arc<OccupancyGrid>>,
}

pub struct MapRuntime {
    planar_frame_id: FrameId,
    body_radius_m: f64,
    store: RevisionStore,
    submap_store: SubmapStore,
    current_submap_occupancy: Option<OccupancyGrid>,
    view: ReadCell<MapView>,
    query_view_committed: bool,
    mapping_sensor_poses: BTreeMap<CapabilityRef, ResolvedSensorPose>,
    latest_robot_pose: Option<PoseEstimate>,
    revision_publisher: Publisher<Stamped<MapRevision>>,
    summary_publisher: Publisher<Stamped<Summary>>,
    local_cost_publisher: Publisher<Stamped<LocalCost>>,
    traversability_publisher: Publisher<Stamped<Traversability>>,
    traversability_summary_publisher: Publisher<Stamped<TraversabilitySummary>>,
}

#[async_trait::async_trait]
impl Runtime for MapRuntime {
    const RUNTIME_ID: &'static str = "map";

    type Args = EmptyArgs;
    type Config = Config;
    type Input = Input;

    fn config(_args: &Self::Args, common: &RobotRuntimeArgs) -> Result<Self::Config> {
        Config::from_args(common)
    }

    fn clock_period(config: &Self::Config) -> Duration {
        config.clock_period()
    }

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self> {
        io.subscribe::<Stamped<LocalizationState>, _>(
            &localize_state::path(),
            Input::LocalizationState,
        )
        .await?;
        io.subscribe::<Stamped<LocalizationRevision>, _>(
            &localize_revision::path(),
            Input::LocalizationRevision,
        )
        .await?;
        io.subscribe::<Stamped<Keyframe>, _>(&localize_keyframe::path(), Input::Keyframe)
            .await?;
        if config.mapping_range_inputs.is_empty() {
            info!(
                "map runtime has no range sensors tagged for mapping; submap occupancy will remain Unknown"
            );
        }
        for capability_ref in &config.mapping_range_inputs {
            let topic = range::path(&capability_ref.component_id, &capability_ref.capability_id);
            let sensor = capability_ref.clone();
            io.subscribe::<Stamped<range::Sample>, _>(&topic, move |sample| {
                Input::RangeSample(sensor.clone(), sample)
            })
            .await?;
        }
        let store = RevisionStore::new();
        let submap_store = SubmapStore::new();
        let current_submap_occupancy: Option<OccupancyGrid> = None;
        let view = ReadCell::new(MapView {
            planar_frame_id: config.planar_frame_id.clone(),
            body_radius_m: config.body_radius_m,
            store: store.clone(),
            submaps: submap_store.clone(),
            occupancy: current_submap_occupancy
                .as_ref()
                .map(|grid| Arc::new(grid.clone())),
        });

        io.serve_query::<SubmapRequest, SubmapResponse, MapView, _>(
            &submap::path(),
            view.reader(),
            map_query_options(),
            map_submap,
        )
        .await?;
        io.serve_query::<EsdfTileRequest, EsdfTileResponse, MapView, _>(
            &esdf_tile::path(),
            view.reader(),
            map_query_options(),
            map_esdf_tile,
        )
        .await?;
        io.serve_query::<TraversabilityTileRequest, TraversabilityTileResponse, MapView, _>(
            &traversability_tile::path(),
            view.reader(),
            map_query_options(),
            map_traversability_tile,
        )
        .await?;
        io.serve_query::<LocalGridRequest, LocalGridResponse, MapView, _>(
            &local_grid::path(),
            view.reader(),
            map_query_options(),
            map_local_grid,
        )
        .await?;
        io.serve_query::<GlobalGridRequest, GlobalGridResponse, MapView, _>(
            &global_grid::path(),
            view.reader(),
            map_query_options(),
            map_global_grid,
        )
        .await?;
        io.serve_query::<SnapshotRequest, SnapshotResponse, MapView, _>(
            &snapshot::path(),
            view.reader(),
            map_query_options(),
            map_snapshot,
        )
        .await?;

        Ok(Self {
            planar_frame_id: config.planar_frame_id,
            body_radius_m: config.body_radius_m,
            store,
            submap_store,
            current_submap_occupancy,
            view,
            query_view_committed: false,
            mapping_sensor_poses: config.mapping_sensor_poses,
            latest_robot_pose: None,
            revision_publisher: io
                .publisher::<Stamped<MapRevision>>(&revision::path())
                .await?,
            summary_publisher: io.publisher::<Stamped<Summary>>(&summary::path()).await?,
            local_cost_publisher: io
                .publisher::<Stamped<LocalCost>>(&local_cost::path())
                .await?,
            traversability_publisher: io
                .publisher::<Stamped<Traversability>>(&traversability::path())
                .await?,
            traversability_summary_publisher: io
                .publisher::<Stamped<TraversabilitySummary>>(&traversability_summary::path())
                .await?,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        let timestamp_ns = step.tick.time_ns();
        let mut store_changed = false;
        let mut submaps_changed = false;
        let mut occupancy_changed = false;
        for input in inputs {
            match input {
                Input::LocalizationState(sample) => {
                    self.latest_robot_pose = sample.data.pose.clone();
                }
                Input::LocalizationRevision(sample) => {
                    if let Some(retained) = self.store.observe(&sample.data) {
                        store_changed = true;
                        self.revision_publisher
                            .put(&Stamped::new(timestamp_ns, map_revision_payload(&retained)))
                            .await?;
                    }
                }
                Input::Keyframe(sample) => {
                    if self.submap_store.ingest(&sample.data).is_some() {
                        submaps_changed = true;
                        occupancy_changed = self.current_submap_occupancy.is_some();
                        self.current_submap_occupancy = None;
                    }
                }
                Input::RangeSample(capability_ref, sample) => {
                    let Some(latest) = self.submap_store.latest() else {
                        continue;
                    };
                    let Some(sensor_pose) = self.mapping_sensor_poses.get(&capability_ref) else {
                        continue;
                    };
                    let Some(robot_pose) = self.latest_robot_pose.clone() else {
                        continue;
                    };
                    let grid = self.current_submap_occupancy.get_or_insert_with(|| {
                        OccupancyGrid::centered_at([
                            latest.anchor_translation_m[0],
                            latest.anchor_translation_m[1],
                        ])
                    });
                    occupancy_changed = true;
                    let robot_yaw = yaw_from_quaternion_xyzw(robot_pose.rotation_xyzw);
                    let off_x = f64::from(sensor_pose.offset_xyz_m[0]);
                    let off_y = f64::from(sensor_pose.offset_xyz_m[1]);
                    let sensor_origin = [
                        robot_pose.translation_m[0]
                            + (robot_yaw.cos() * off_x - robot_yaw.sin() * off_y),
                        robot_pose.translation_m[1]
                            + (robot_yaw.sin() * off_x + robot_yaw.cos() * off_y),
                    ];
                    let beam_yaw = robot_yaw + f64::from(sensor_pose.yaw_rad);
                    let sample_distance_m = sample.data.distance_m();
                    let max_range_m = range_max_m(sensor_pose, sample_distance_m);
                    for ray in sample_range_rays(beam_yaw as f32, max_range_m, sample_distance_m) {
                        grid.integrate_ray(
                            sensor_origin,
                            f64::from(ray.angle_rad),
                            f64::from(ray.clear_distance_m),
                            ray.occupied_distance_m.map(f64::from),
                        );
                    }
                }
            }
        }

        if let Some(current) = self.store.current() {
            self.local_cost_publisher
                .put(&Stamped::new(
                    timestamp_ns,
                    local_cost_payload(
                        current.map_revision_id,
                        current.built_from_localize_revision,
                        &self.planar_frame_id,
                    ),
                ))
                .await?;
            if let Some(occupancy) = self.current_submap_occupancy.as_ref() {
                self.traversability_publisher
                    .put(&Stamped::new(
                        timestamp_ns,
                        traversability_payload(
                            current.map_revision_id,
                            current.built_from_localize_revision,
                            &self.planar_frame_id,
                            occupancy.traversability(self.body_radius_m),
                        ),
                    ))
                    .await?;
            }
            self.traversability_summary_publisher
                .put(&Stamped::new(
                    timestamp_ns,
                    traversability_summary_payload(
                        current.map_revision_id,
                        current.built_from_localize_revision,
                        &self.planar_frame_id,
                        self.current_submap_occupancy.is_some(),
                    ),
                ))
                .await?;
        }

        self.summary_publisher
            .put(&Stamped::new(
                timestamp_ns,
                summary_payload(
                    self.store
                        .current()
                        .map(|revision| revision.map_revision_id),
                    self.store
                        .current()
                        .map(|revision| revision.built_from_localize_revision),
                    &self.planar_frame_id,
                ),
            ))
            .await?;

        self.commit_query_view(store_changed, submaps_changed, occupancy_changed);

        Ok(())
    }

    fn scenarios() -> &'static [phoxal::runtime::runtime::ScenarioDescriptor] {
        crate::scenarios::SCENARIOS
    }

    async fn run_scenario(name: &str, common: &RobotRuntimeArgs, _args: &Self::Args) -> Result<()> {
        crate::scenarios::run(name, common).await
    }
}

impl MapRuntime {
    fn commit_query_view(
        &mut self,
        store_changed: bool,
        submaps_changed: bool,
        occupancy_changed: bool,
    ) {
        if self.query_view_committed && !store_changed && !submaps_changed && !occupancy_changed {
            return;
        }

        let previous = self.view.load();
        let occupancy = if occupancy_changed || !self.query_view_committed {
            self.current_submap_occupancy
                .as_ref()
                .map(|grid| Arc::new(grid.clone()))
        } else {
            previous.occupancy.clone()
        };

        self.view.publish(MapView {
            planar_frame_id: self.planar_frame_id.clone(),
            body_radius_m: self.body_radius_m,
            store: self.store.clone(),
            submaps: self.submap_store.clone(),
            occupancy,
        });
        self.query_view_committed = true;
    }
}

fn map_query_options() -> QueryOptions {
    QueryOptions::max_in_flight(NonZeroUsize::new(4).unwrap())
}

fn map_submap(view: &MapView, request: SubmapRequest) -> SubmapResponse {
    submap_response(
        &request,
        &view.store,
        &view.submaps,
        view.occupancy.as_deref(),
        &view.planar_frame_id,
    )
}

fn map_esdf_tile(view: &MapView, request: EsdfTileRequest) -> EsdfTileResponse {
    esdf_tile_response(&request, &view.store, &view.planar_frame_id)
}

fn map_traversability_tile(
    view: &MapView,
    request: TraversabilityTileRequest,
) -> TraversabilityTileResponse {
    traversability_tile_response(
        &request,
        &view.store,
        view.occupancy.as_deref(),
        &view.planar_frame_id,
        view.body_radius_m,
    )
}

fn map_local_grid(view: &MapView, request: LocalGridRequest) -> LocalGridResponse {
    local_grid_response(
        &request,
        &view.store,
        &view.submaps,
        view.occupancy.as_deref(),
        &view.planar_frame_id,
    )
}

fn map_global_grid(view: &MapView, request: GlobalGridRequest) -> GlobalGridResponse {
    global_grid_response(&request, &view.store, &view.planar_frame_id)
}

fn map_snapshot(view: &MapView, request: SnapshotRequest) -> SnapshotResponse {
    snapshot_response(&request, &view.store, &view.submaps, &view.planar_frame_id)
}

fn map_revision_payload(retained: &RetainedRevision) -> MapRevision {
    MapRevision {
        map_revision_id: retained.map_revision_id,
        previous_map_revision_id: retained.previous_map_revision_id,
        built_from_localize_revision: retained.built_from_localize_revision,
        cause: retained.cause,
        affected_region: None,
    }
}

fn summary_payload(
    current_revision: Option<MapRevisionId>,
    built_from_localize_revision: Option<LocalizationRevisionId>,
    frame_id: &FrameId,
) -> Summary {
    Summary {
        current_revision,
        built_from_localize_revision,
        frame_id: frame_id.clone(),
        known_region: None,
    }
}

fn local_cost_payload(
    map_revision: MapRevisionId,
    localize_revision: LocalizationRevisionId,
    frame_id: &FrameId,
) -> LocalCost {
    LocalCost {
        map_revision,
        built_from_localize_revision: localize_revision,
        frame_id: frame_id.clone(),
        grid: zero_grid(0.0),
    }
}

fn traversability_payload(
    map_revision: MapRevisionId,
    localize_revision: LocalizationRevisionId,
    frame_id: &FrameId,
    cells: Grid<TraversabilityCell>,
) -> Traversability {
    Traversability {
        map_revision,
        built_from_localize_revision: localize_revision,
        frame_id: frame_id.clone(),
        cells,
    }
}

fn traversability_summary_payload(
    map_revision: MapRevisionId,
    localize_revision: LocalizationRevisionId,
    frame_id: &FrameId,
    occupancy_available: bool,
) -> TraversabilitySummary {
    TraversabilitySummary {
        map_revision,
        built_from_localize_revision: localize_revision,
        frame_id: frame_id.clone(),
        region: empty_region(frame_id),
        status: if occupancy_available {
            TraversabilityStatus::Ready
        } else {
            TraversabilityStatus::Unavailable
        },
    }
}

fn zero_grid<T>(cell: T) -> Grid<T> {
    Grid {
        origin_xy_m: [0.0, 0.0],
        resolution: phoxal::api::map::v1::Resolution {
            xy_m: 1.0,
            z_m: None,
        },
        width_cells: 1,
        height_cells: 1,
        cells: vec![cell],
    }
}

fn empty_region(frame_id: &FrameId) -> RegionSummary {
    RegionSummary {
        frame_id: frame_id.clone(),
        min_xyz_m: [0.0, 0.0, 0.0],
        max_xyz_m: [0.0, 0.0, 0.0],
    }
}

fn yaw_from_quaternion_xyzw(rotation_xyzw: [f64; 4]) -> f64 {
    let [x, y, z, w] = rotation_xyzw;
    if !rotation_xyzw.iter().all(|value| value.is_finite()) {
        return 0.0;
    }
    let norm_squared = x.mul_add(x, y.mul_add(y, z.mul_add(z, w * w)));
    if norm_squared <= f64::EPSILON {
        return 0.0;
    }

    UnitQuaternion::from_quaternion(Quaternion::new(w, x, y, z))
        .euler_angles()
        .2
}

fn range_max_m(sensor_pose: &ResolvedSensorPose, sample_distance_m: f32) -> f32 {
    match &sensor_pose.kind {
        ResolvedSensorKind::Range { max_range_m, .. } => *max_range_m,
        _ => sample_distance_m,
    }
}

fn tile_response<T>(
    lookup: RevisionLookup,
    frame_id: &FrameId,
    payload: impl FnOnce(&RetainedRevision) -> T,
) -> MapTileResponse<T> {
    match lookup {
        RevisionLookup::Found(retained) => MapTileResponse::Ok {
            served_map_revision: retained.map_revision_id,
            built_from_localize_revision: retained.built_from_localize_revision,
            frame_id: frame_id.clone(),
            payload: payload(&retained),
        },
        RevisionLookup::Stale { current } => MapTileResponse::StaleRevision { current },
        RevisionLookup::Unavailable { latest_available } => {
            MapTileResponse::RevisionUnavailable { latest_available }
        }
        RevisionLookup::WrongEpoch { current } => MapTileResponse::WrongEpoch { current },
    }
}

fn submap_response(
    request: &SubmapRequest,
    store: &RevisionStore,
    submaps: &SubmapStore,
    occupancy: Option<&OccupancyGrid>,
    frame_id: &FrameId,
) -> SubmapResponse {
    match store.lookup(request.0.requested_revision) {
        RevisionLookup::Found(retained) => match submaps.latest() {
            Some(latest) => {
                let bytes = match occupancy {
                    Some(grid) => rmp_serde::to_vec(&grid.to_snapshot())
                        .expect("occupancy snapshot should serialize to MessagePack"),
                    None => Vec::new(),
                };
                SubmapResponse(MapTileResponse::Ok {
                    served_map_revision: retained.map_revision_id,
                    built_from_localize_revision: retained.built_from_localize_revision,
                    frame_id: frame_id.clone(),
                    payload: phoxal::api::map::v1::Submap {
                        submap_id: latest.submap_id.clone(),
                        bytes,
                    },
                })
            }
            None => SubmapResponse(MapTileResponse::RegionUnavailable {
                served_map_revision: retained.map_revision_id,
            }),
        },
        RevisionLookup::Stale { current } => {
            SubmapResponse(MapTileResponse::StaleRevision { current })
        }
        RevisionLookup::Unavailable { latest_available } => {
            SubmapResponse(MapTileResponse::RevisionUnavailable { latest_available })
        }
        RevisionLookup::WrongEpoch { current } => {
            SubmapResponse(MapTileResponse::WrongEpoch { current })
        }
    }
}

fn esdf_tile_response(
    request: &EsdfTileRequest,
    store: &RevisionStore,
    frame_id: &FrameId,
) -> EsdfTileResponse {
    EsdfTileResponse(tile_response(
        store.lookup(request.0.requested_revision),
        frame_id,
        |_| empty_esdf_tile(),
    ))
}

fn traversability_tile_response(
    request: &TraversabilityTileRequest,
    store: &RevisionStore,
    occupancy: Option<&OccupancyGrid>,
    frame_id: &FrameId,
    body_radius_m: f64,
) -> TraversabilityTileResponse {
    match store.lookup(request.0.requested_revision) {
        RevisionLookup::Found(retained) => match occupancy {
            Some(grid) => TraversabilityTileResponse(MapTileResponse::Ok {
                served_map_revision: retained.map_revision_id,
                built_from_localize_revision: retained.built_from_localize_revision,
                frame_id: frame_id.clone(),
                payload: TraversabilityTile {
                    cells: grid.traversability(body_radius_m),
                },
            }),
            None => TraversabilityTileResponse(MapTileResponse::RegionUnavailable {
                served_map_revision: retained.map_revision_id,
            }),
        },
        RevisionLookup::Stale { current } => {
            TraversabilityTileResponse(MapTileResponse::StaleRevision { current })
        }
        RevisionLookup::Unavailable { latest_available } => {
            TraversabilityTileResponse(MapTileResponse::RevisionUnavailable { latest_available })
        }
        RevisionLookup::WrongEpoch { current } => {
            TraversabilityTileResponse(MapTileResponse::WrongEpoch { current })
        }
    }
}

fn local_grid_response(
    request: &LocalGridRequest,
    store: &RevisionStore,
    submaps: &SubmapStore,
    occupancy: Option<&OccupancyGrid>,
    frame_id: &FrameId,
) -> LocalGridResponse {
    match store.lookup(request.0.requested_revision) {
        RevisionLookup::Found(retained) => match (submaps.latest(), occupancy) {
            (Some(_), Some(grid)) => LocalGridResponse(MapTileResponse::Ok {
                served_map_revision: retained.map_revision_id,
                built_from_localize_revision: retained.built_from_localize_revision,
                frame_id: frame_id.clone(),
                payload: LocalGrid {
                    cells: grid.to_local_grid(),
                },
            }),
            (None, _) | (Some(_), None) => LocalGridResponse(MapTileResponse::RegionUnavailable {
                served_map_revision: retained.map_revision_id,
            }),
        },
        RevisionLookup::Stale { current } => {
            LocalGridResponse(MapTileResponse::StaleRevision { current })
        }
        RevisionLookup::Unavailable { latest_available } => {
            LocalGridResponse(MapTileResponse::RevisionUnavailable { latest_available })
        }
        RevisionLookup::WrongEpoch { current } => {
            LocalGridResponse(MapTileResponse::WrongEpoch { current })
        }
    }
}

fn global_grid_response(
    request: &GlobalGridRequest,
    store: &RevisionStore,
    frame_id: &FrameId,
) -> GlobalGridResponse {
    GlobalGridResponse(tile_response(
        store.lookup(request.0.requested_revision),
        frame_id,
        |_| empty_global_grid(),
    ))
}

fn snapshot_response(
    request: &SnapshotRequest,
    store: &RevisionStore,
    submaps: &SubmapStore,
    frame_id: &FrameId,
) -> SnapshotResponse {
    match store.lookup(request.0.requested_revision) {
        RevisionLookup::Found(retained) => {
            let known_submaps = submaps.all();
            let mut snapshot_submaps = Vec::with_capacity(submaps.len());
            snapshot_submaps.extend(
                known_submaps
                    .into_iter()
                    .map(|metadata| metadata.to_empty_submap()),
            );
            let available_bytes = snapshot_submaps
                .iter()
                .map(|submap| submap.bytes.len() as u64)
                .sum::<u64>();
            if request
                .0
                .max_bytes
                .is_some_and(|max_bytes| available_bytes > u64::from(max_bytes))
            {
                return SnapshotResponse(MapTileResponse::ResponseTooLarge { available_bytes });
            }

            SnapshotResponse(MapTileResponse::Ok {
                served_map_revision: retained.map_revision_id,
                built_from_localize_revision: retained.built_from_localize_revision,
                frame_id: frame_id.clone(),
                payload: Snapshot {
                    map_revision: retained.map_revision_id,
                    submaps: snapshot_submaps,
                },
            })
        }
        RevisionLookup::Stale { current } => {
            SnapshotResponse(MapTileResponse::StaleRevision { current })
        }
        RevisionLookup::Unavailable { latest_available } => {
            SnapshotResponse(MapTileResponse::RevisionUnavailable { latest_available })
        }
        RevisionLookup::WrongEpoch { current } => {
            SnapshotResponse(MapTileResponse::WrongEpoch { current })
        }
    }
}

fn empty_esdf_tile() -> EsdfTile {
    EsdfTile {
        distances_m: empty_grid(),
    }
}

fn empty_global_grid() -> GlobalGrid {
    GlobalGrid {
        cells: empty_grid(),
    }
}

fn empty_grid<T>() -> Grid<T> {
    Grid {
        origin_xy_m: [0.0, 0.0],
        resolution: phoxal::api::map::v1::Resolution {
            xy_m: 1.0,
            z_m: None,
        },
        width_cells: 0,
        height_cells: 0,
        cells: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use phoxal::api::localize::v1::{
        AffectedKeyframeSummary, Keyframe, KeyframeId, LocalizationRevisionCause, PoseEstimate,
        Region as LocalizeRegion,
    };
    use phoxal::api::map::v1::{MapRevisionCause, MapTileRequest, Region, Resolution, Submap};

    use crate::core::occupancy::{
        GRID_HEIGHT_CELLS, GRID_RESOLUTION_M, GRID_WIDTH_CELLS, OccupancyGrid, OccupancySnapshot,
    };
    use crate::core::revisions::INITIAL_MAP_EPOCH;

    use super::*;

    #[test]
    fn summary_has_no_revision_before_localization_arrives() {
        let frame_id = FrameId::new("map");

        let summary = summary_payload(None, None, &frame_id);

        assert_eq!(summary.current_revision, None);
    }

    #[test]
    fn summary_reports_revision_linkage_after_revision_observation() {
        let mut store = RevisionStore::new();
        let localize_revision = localize_revision_id(1, 7);
        let Some(retained) = store.observe(&localization_revision(localize_revision)) else {
            panic!("localize revision should activate map");
        };

        let summary = summary_payload(
            Some(retained.map_revision_id),
            Some(retained.built_from_localize_revision),
            &FrameId::new("map"),
        );

        assert_eq!(summary.current_revision, Some(initial_map_revision()));
        assert_eq!(
            summary.built_from_localize_revision,
            Some(localize_revision)
        );
    }

    #[test]
    fn revision_subscription_emits_map_revision_with_linkage() {
        let mut store = RevisionStore::new();

        let Some(first) = store
            .observe(&localization_revision(localize_revision_id(1, 0)))
            .map(|retained| map_revision_payload(&retained))
        else {
            panic!("first localize revision should publish map revision");
        };
        let Some(second) = store
            .observe(&localization_revision(localize_revision_id(1, 1)))
            .map(|retained| map_revision_payload(&retained))
        else {
            panic!("second localize revision should publish map revision");
        };

        assert_eq!(first.map_revision_id, initial_map_revision());
        assert_eq!(first.previous_map_revision_id, None);
        assert_eq!(first.cause, MapRevisionCause::SensorIntegration);
        assert_eq!(second.map_revision_id.sequence, 1);
        assert_eq!(second.previous_map_revision_id, Some(first.map_revision_id));
        assert_eq!(second.cause, MapRevisionCause::LocalizationCorrection);
    }

    #[test]
    fn every_query_serves_retained_revision_with_empty_payload() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localization_revision(localize_revision_id(1, 0)))
        else {
            panic!("revision should be retained");
        };
        let request = map_tile_request();
        let frame_id = FrameId::new("map");
        let submaps = SubmapStore::new();

        assert_eq!(
            submap_response_for(
                &SubmapRequest(request.clone()),
                &store,
                &submaps,
                None,
                &frame_id
            ),
            SubmapResponse(MapTileResponse::<Submap>::RegionUnavailable {
                served_map_revision: retained.map_revision_id,
            })
        );
        assert_eq!(
            esdf_tile_response(&EsdfTileRequest(request.clone()), &store, &frame_id),
            EsdfTileResponse(MapTileResponse::<EsdfTile>::Ok {
                served_map_revision: retained.map_revision_id,
                built_from_localize_revision: retained.built_from_localize_revision,
                frame_id: frame_id.clone(),
                payload: empty_esdf_tile()
            })
        );
        assert_eq!(
            traversability_tile_response(
                &TraversabilityTileRequest(request.clone()),
                &store,
                None,
                &frame_id,
                0.30
            ),
            TraversabilityTileResponse(MapTileResponse::<TraversabilityTile>::RegionUnavailable {
                served_map_revision: retained.map_revision_id,
            })
        );
        assert_eq!(
            local_grid_response(
                &LocalGridRequest(request.clone()),
                &store,
                &submaps,
                None,
                &frame_id
            ),
            LocalGridResponse(MapTileResponse::<LocalGrid>::RegionUnavailable {
                served_map_revision: retained.map_revision_id,
            })
        );
        assert_eq!(
            global_grid_response(&GlobalGridRequest(request.clone()), &store, &frame_id),
            GlobalGridResponse(MapTileResponse::<GlobalGrid>::Ok {
                served_map_revision: retained.map_revision_id,
                built_from_localize_revision: retained.built_from_localize_revision,
                frame_id: frame_id.clone(),
                payload: empty_global_grid()
            })
        );
        assert_eq!(
            snapshot_response(&SnapshotRequest(request), &store, &submaps, &frame_id),
            SnapshotResponse(MapTileResponse::<Snapshot>::Ok {
                served_map_revision: retained.map_revision_id,
                built_from_localize_revision: retained.built_from_localize_revision,
                frame_id,
                payload: Snapshot {
                    map_revision: retained.map_revision_id,
                    submaps: Vec::new()
                }
            })
        );
    }

    #[test]
    fn map_handlers_match_response_builders_for_available_view() {
        let mut store = RevisionStore::new();
        let Some(_) = store.observe(&localization_revision(localize_revision_id(1, 0))) else {
            panic!("revision should be retained");
        };
        let mut submaps = SubmapStore::new();
        let _ = submaps.ingest(&keyframe("kf-a", 0));
        let mut grid = OccupancyGrid::centered_at([0.0, 0.0]);
        grid.integrate_ray([0.0, 0.0], 0.0, 1.0, Some(1.0));
        let view = map_view(store, submaps, Some(grid), 0.30);
        let request = map_tile_request();

        assert_eq!(
            map_submap(&view, SubmapRequest(request.clone())),
            submap_response(
                &SubmapRequest(request.clone()),
                &view.store,
                &view.submaps,
                view.occupancy.as_deref(),
                &view.planar_frame_id,
            )
        );
        assert_eq!(
            map_esdf_tile(&view, EsdfTileRequest(request.clone())),
            esdf_tile_response(
                &EsdfTileRequest(request.clone()),
                &view.store,
                &view.planar_frame_id,
            )
        );
        assert_eq!(
            map_traversability_tile(&view, TraversabilityTileRequest(request.clone())),
            traversability_tile_response(
                &TraversabilityTileRequest(request.clone()),
                &view.store,
                view.occupancy.as_deref(),
                &view.planar_frame_id,
                view.body_radius_m,
            )
        );
        assert_eq!(
            map_local_grid(&view, LocalGridRequest(request.clone())),
            local_grid_response(
                &LocalGridRequest(request.clone()),
                &view.store,
                &view.submaps,
                view.occupancy.as_deref(),
                &view.planar_frame_id,
            )
        );
        assert_eq!(
            map_global_grid(&view, GlobalGridRequest(request.clone())),
            global_grid_response(
                &GlobalGridRequest(request.clone()),
                &view.store,
                &view.planar_frame_id,
            )
        );
        assert_eq!(
            map_snapshot(&view, SnapshotRequest(request.clone())),
            snapshot_response(
                &SnapshotRequest(request),
                &view.store,
                &view.submaps,
                &view.planar_frame_id,
            )
        );
    }

    #[test]
    fn map_handlers_match_response_builders_for_unavailable_regions() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localization_revision(localize_revision_id(1, 0)))
        else {
            panic!("revision should be retained");
        };
        let view = map_view(store, SubmapStore::new(), None, 0.30);
        let request = map_tile_request();

        let submap = map_submap(&view, SubmapRequest(request.clone()));
        assert_eq!(
            submap,
            submap_response(
                &SubmapRequest(request.clone()),
                &view.store,
                &view.submaps,
                view.occupancy.as_deref(),
                &view.planar_frame_id,
            )
        );
        assert_eq!(
            submap,
            SubmapResponse(MapTileResponse::<Submap>::RegionUnavailable {
                served_map_revision: retained.map_revision_id,
            })
        );

        let local_grid = map_local_grid(&view, LocalGridRequest(request.clone()));
        assert_eq!(
            local_grid,
            local_grid_response(
                &LocalGridRequest(request.clone()),
                &view.store,
                &view.submaps,
                view.occupancy.as_deref(),
                &view.planar_frame_id,
            )
        );
        assert_eq!(
            local_grid,
            LocalGridResponse(MapTileResponse::<LocalGrid>::RegionUnavailable {
                served_map_revision: retained.map_revision_id,
            })
        );

        let traversability =
            map_traversability_tile(&view, TraversabilityTileRequest(request.clone()));
        assert_eq!(
            traversability,
            traversability_tile_response(
                &TraversabilityTileRequest(request),
                &view.store,
                view.occupancy.as_deref(),
                &view.planar_frame_id,
                view.body_radius_m,
            )
        );
        assert_eq!(
            traversability,
            TraversabilityTileResponse(MapTileResponse::<TraversabilityTile>::RegionUnavailable {
                served_map_revision: retained.map_revision_id,
            })
        );
    }

    #[test]
    fn map_snapshot_handler_matches_builder_for_revision_errors() {
        let mut store = RevisionStore::new();
        for sequence in 0..5 {
            let _ = store.observe(&localization_revision(localize_revision_id(1, sequence)));
        }
        let view = map_view(store, SubmapStore::new(), None, 0.30);

        let stale_request = map_tile_request_for(MapRevisionId {
            epoch: INITIAL_MAP_EPOCH,
            sequence: 0,
        });
        let stale = map_snapshot(&view, SnapshotRequest(stale_request.clone()));
        assert_eq!(
            stale,
            snapshot_response(
                &SnapshotRequest(stale_request),
                &view.store,
                &view.submaps,
                &view.planar_frame_id,
            )
        );
        assert_eq!(
            stale,
            SnapshotResponse(MapTileResponse::<Snapshot>::StaleRevision {
                current: MapRevisionId {
                    epoch: INITIAL_MAP_EPOCH,
                    sequence: 4,
                },
            })
        );

        let unavailable_request = map_tile_request_for(MapRevisionId {
            epoch: INITIAL_MAP_EPOCH,
            sequence: 9,
        });
        let unavailable = map_snapshot(&view, SnapshotRequest(unavailable_request.clone()));
        assert_eq!(
            unavailable,
            snapshot_response(
                &SnapshotRequest(unavailable_request),
                &view.store,
                &view.submaps,
                &view.planar_frame_id,
            )
        );
        assert_eq!(
            unavailable,
            SnapshotResponse(MapTileResponse::<Snapshot>::RevisionUnavailable {
                latest_available: Some(MapRevisionId {
                    epoch: INITIAL_MAP_EPOCH,
                    sequence: 4,
                }),
            })
        );

        let wrong_epoch_request = map_tile_request_for(MapRevisionId {
            epoch: 99,
            sequence: 0,
        });
        let wrong_epoch = map_snapshot(&view, SnapshotRequest(wrong_epoch_request.clone()));
        assert_eq!(
            wrong_epoch,
            snapshot_response(
                &SnapshotRequest(wrong_epoch_request),
                &view.store,
                &view.submaps,
                &view.planar_frame_id,
            )
        );
        assert_eq!(
            wrong_epoch,
            SnapshotResponse(MapTileResponse::<Snapshot>::WrongEpoch {
                current: MapRevisionId {
                    epoch: INITIAL_MAP_EPOCH,
                    sequence: 4,
                },
            })
        );
    }

    #[test]
    fn submap_response_returns_region_unavailable_when_store_empty() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localization_revision(localize_revision_id(1, 0)))
        else {
            panic!("revision should be retained");
        };
        let submaps = SubmapStore::new();
        let frame_id = FrameId::new("map");

        assert_eq!(
            submap_response_for(
                &SubmapRequest(map_tile_request()),
                &store,
                &submaps,
                None,
                &frame_id
            ),
            SubmapResponse(MapTileResponse::<Submap>::RegionUnavailable {
                served_map_revision: retained.map_revision_id,
            })
        );
    }

    #[test]
    fn submap_response_returns_ok_with_latest_after_keyframe_ingested() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localization_revision(localize_revision_id(1, 0)))
        else {
            panic!("revision should be retained");
        };
        let mut submaps = SubmapStore::new();
        let _ = submaps.ingest(&keyframe("kf-a", 0));
        let frame_id = FrameId::new("map");

        assert_eq!(
            submap_response_for(
                &SubmapRequest(map_tile_request()),
                &store,
                &submaps,
                None,
                &frame_id
            ),
            SubmapResponse(MapTileResponse::<Submap>::Ok {
                served_map_revision: retained.map_revision_id,
                built_from_localize_revision: retained.built_from_localize_revision,
                frame_id,
                payload: Submap {
                    submap_id: phoxal::api::map::v1::SubmapId::new("submap-kf-a"),
                    bytes: Vec::new()
                }
            })
        );
    }

    #[test]
    fn local_grid_response_returns_ok_with_occupancy_after_range_samples() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localization_revision(localize_revision_id(1, 0)))
        else {
            panic!("revision should be retained");
        };
        let mut submaps = SubmapStore::new();
        let _ = submaps.ingest(&keyframe("kf-a", 0));
        let mut grid = OccupancyGrid::centered_at([0.0, 0.0]);
        grid.integrate_ray([0.0, 0.0], 0.0, 1.0, Some(1.0));
        let frame_id = FrameId::new("map");

        assert_eq!(
            local_grid_response(
                &LocalGridRequest(map_tile_request()),
                &store,
                &submaps,
                Some(&grid),
                &frame_id
            ),
            LocalGridResponse(MapTileResponse::<LocalGrid>::Ok {
                served_map_revision: retained.map_revision_id,
                built_from_localize_revision: retained.built_from_localize_revision,
                frame_id,
                payload: LocalGrid {
                    cells: grid.to_local_grid()
                }
            })
        );
    }

    #[test]
    fn local_grid_response_returns_region_unavailable_before_any_range_sample() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localization_revision(localize_revision_id(1, 0)))
        else {
            panic!("revision should be retained");
        };
        let mut submaps = SubmapStore::new();
        let _ = submaps.ingest(&keyframe("kf-a", 0));
        let frame_id = FrameId::new("map");

        assert_eq!(
            local_grid_response(
                &LocalGridRequest(map_tile_request()),
                &store,
                &submaps,
                None,
                &frame_id
            ),
            LocalGridResponse(MapTileResponse::<LocalGrid>::RegionUnavailable {
                served_map_revision: retained.map_revision_id,
            })
        );
    }

    #[test]
    fn traversability_tile_response_returns_ok_with_inflated_cells_after_range_samples() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localization_revision(localize_revision_id(1, 0)))
        else {
            panic!("revision should be retained");
        };
        let mut submaps = SubmapStore::new();
        let _ = submaps.ingest(&keyframe("kf-a", 0));
        let mut grid = OccupancyGrid::centered_at([0.0, 0.0]);
        grid.integrate_ray([0.0, 0.0], 0.0, 1.0, Some(1.0));
        let frame_id = FrameId::new("map");

        let response = traversability_tile_response(
            &TraversabilityTileRequest(map_tile_request()),
            &store,
            Some(&grid),
            &frame_id,
            0.30,
        );

        let TraversabilityTileResponse(MapTileResponse::Ok {
            served_map_revision,
            payload,
            ..
        }) = response
        else {
            panic!("expected traversability tile response");
        };
        assert_eq!(served_map_revision, retained.map_revision_id);
        assert!(payload.cells.cells.contains(&TraversabilityCell::Inflated));
        assert_eq!(
            payload.cells.cells[index(terminal_x_cell(1.0), sensor_y_cell())],
            TraversabilityCell::Occupied
        );
    }

    #[test]
    fn traversability_tile_response_returns_region_unavailable_before_any_range_sample() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localization_revision(localize_revision_id(1, 0)))
        else {
            panic!("revision should be retained");
        };
        let mut submaps = SubmapStore::new();
        let _ = submaps.ingest(&keyframe("kf-a", 0));
        let frame_id = FrameId::new("map");

        assert_eq!(
            traversability_tile_response(
                &TraversabilityTileRequest(map_tile_request()),
                &store,
                None,
                &frame_id,
                0.30,
            ),
            TraversabilityTileResponse(MapTileResponse::<TraversabilityTile>::RegionUnavailable {
                served_map_revision: retained.map_revision_id,
            })
        );
    }

    #[test]
    fn traversability_summary_reports_ready_when_occupancy_exists() {
        let summary = traversability_summary_payload(
            initial_map_revision(),
            localize_revision_id(1, 0),
            &FrameId::new("map"),
            true,
        );

        assert_eq!(summary.status, TraversabilityStatus::Ready);
    }

    #[test]
    fn traversability_summary_reports_unavailable_before_any_occupancy() {
        let summary = traversability_summary_payload(
            initial_map_revision(),
            localize_revision_id(1, 0),
            &FrameId::new("map"),
            false,
        );

        assert_eq!(summary.status, TraversabilityStatus::Unavailable);
    }

    #[test]
    fn yaw_from_quaternion_reads_planar_heading() {
        let yaw_rad = std::f64::consts::FRAC_PI_2;
        let rotation = nalgebra::UnitQuaternion::from_euler_angles(0.0, 0.0, yaw_rad);
        let quaternion = rotation.quaternion();

        let extracted_yaw =
            yaw_from_quaternion_xyzw([quaternion.i, quaternion.j, quaternion.k, quaternion.w]);

        assert!((extracted_yaw - yaw_rad).abs() < 1e-9);
    }

    #[test]
    fn yaw_from_quaternion_defaults_degenerate_rotation_to_zero() {
        assert_eq!(yaw_from_quaternion_xyzw([0.0, 0.0, 0.0, 0.0]), 0.0);
        assert_eq!(yaw_from_quaternion_xyzw([f64::NAN, 0.0, 0.0, 1.0]), 0.0);
    }

    #[test]
    fn submap_response_returns_encoded_bytes_after_range_samples() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localization_revision(localize_revision_id(1, 0)))
        else {
            panic!("revision should be retained");
        };
        let mut submaps = SubmapStore::new();
        let _ = submaps.ingest(&keyframe("kf-a", 0));
        let mut grid = OccupancyGrid::centered_at([0.0, 0.0]);
        grid.integrate_ray([0.0, 0.0], 0.0, 1.0, Some(1.0));
        let frame_id = FrameId::new("map");

        let response = submap_response_for(
            &SubmapRequest(map_tile_request()),
            &store,
            &submaps,
            Some(&grid),
            &frame_id,
        );

        let SubmapResponse(MapTileResponse::Ok {
            served_map_revision,
            payload,
            ..
        }) = response
        else {
            panic!("expected encoded submap response");
        };
        assert_eq!(served_map_revision, retained.map_revision_id);
        let snapshot = match rmp_serde::from_slice::<OccupancySnapshot>(&payload.bytes) {
            Ok(snapshot) => snapshot,
            Err(error) => panic!("failed to decode occupancy snapshot: {error:#}"),
        };
        assert_eq!(snapshot.width_cells, 50);
        assert_eq!(snapshot.height_cells, 50);
        assert_eq!(snapshot.cells.len(), 2500);
    }

    #[test]
    fn submap_response_returns_empty_bytes_before_any_range_sample() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localization_revision(localize_revision_id(1, 0)))
        else {
            panic!("revision should be retained");
        };
        let mut submaps = SubmapStore::new();
        let _ = submaps.ingest(&keyframe("kf-a", 0));
        let frame_id = FrameId::new("map");

        let response = submap_response_for(
            &SubmapRequest(map_tile_request()),
            &store,
            &submaps,
            None,
            &frame_id,
        );

        assert_eq!(
            response,
            SubmapResponse(MapTileResponse::<Submap>::Ok {
                served_map_revision: retained.map_revision_id,
                built_from_localize_revision: retained.built_from_localize_revision,
                frame_id,
                payload: Submap {
                    submap_id: phoxal::api::map::v1::SubmapId::new("submap-kf-a"),
                    bytes: Vec::new()
                }
            })
        );
    }

    #[test]
    fn snapshot_response_includes_all_submaps() {
        let mut store = RevisionStore::new();
        let Some(retained) = store.observe(&localization_revision(localize_revision_id(1, 0)))
        else {
            panic!("revision should be retained");
        };
        let mut submaps = SubmapStore::new();
        let _ = submaps.ingest(&keyframe("kf-b", 0));
        let _ = submaps.ingest(&keyframe("kf-a", 1));
        let _ = submaps.ingest(&keyframe("kf-c", 2));
        let frame_id = FrameId::new("map");

        assert_eq!(
            snapshot_response(
                &SnapshotRequest(map_tile_request()),
                &store,
                &submaps,
                &frame_id
            ),
            SnapshotResponse(MapTileResponse::<Snapshot>::Ok {
                served_map_revision: retained.map_revision_id,
                built_from_localize_revision: retained.built_from_localize_revision,
                frame_id,
                payload: Snapshot {
                    map_revision: retained.map_revision_id,
                    submaps: vec![
                        Submap {
                            submap_id: phoxal::api::map::v1::SubmapId::new("submap-kf-b"),
                            bytes: Vec::new()
                        },
                        Submap {
                            submap_id: phoxal::api::map::v1::SubmapId::new("submap-kf-a"),
                            bytes: Vec::new()
                        },
                        Submap {
                            submap_id: phoxal::api::map::v1::SubmapId::new("submap-kf-c"),
                            bytes: Vec::new()
                        },
                    ]
                }
            })
        );
    }

    #[test]
    fn submap_response_returns_stale_for_evicted_revision() {
        let mut store = RevisionStore::new();
        let mut submaps = SubmapStore::new();

        for sequence in 0..5 {
            let _ = store.observe(&localization_revision(localize_revision_id(1, sequence)));
        }
        let _ = submaps.ingest(&keyframe("kf-a", 4));
        let frame_id = FrameId::new("map");

        assert_eq!(
            submap_response_for(
                &SubmapRequest(map_tile_request()),
                &store,
                &submaps,
                None,
                &frame_id
            ),
            SubmapResponse(MapTileResponse::<Submap>::StaleRevision {
                current: MapRevisionId {
                    epoch: INITIAL_MAP_EPOCH,
                    sequence: 4
                }
            })
        );
    }

    fn map_tile_request() -> MapTileRequest {
        map_tile_request_for(initial_map_revision())
    }

    fn map_tile_request_for(requested_revision: MapRevisionId) -> MapTileRequest {
        MapTileRequest {
            requested_revision,
            region: Region {
                min_xyz_m: [0.0, 0.0, 0.0],
                max_xyz_m: [1.0, 1.0, 1.0],
            },
            resolution: Resolution {
                xy_m: 1.0,
                z_m: None,
            },
            frame_id: FrameId::new("map"),
            max_bytes: None,
        }
    }

    fn map_view(
        store: RevisionStore,
        submaps: SubmapStore,
        occupancy: Option<OccupancyGrid>,
        body_radius_m: f64,
    ) -> MapView {
        MapView {
            planar_frame_id: FrameId::new("map"),
            body_radius_m,
            store,
            submaps,
            occupancy: occupancy.map(std::sync::Arc::new),
        }
    }

    fn localization_revision(revision_id: LocalizationRevisionId) -> LocalizationRevision {
        LocalizationRevision {
            revision_id,
            previous_revision_id: revision_id.sequence.checked_sub(1).map(|sequence| {
                LocalizationRevisionId {
                    epoch: revision_id.epoch,
                    sequence,
                }
            }),
            cause: LocalizationRevisionCause::SensorIntegration,
            affected_keyframes: AffectedKeyframeSummary {
                keyframe_ids: Vec::new(),
                region: Some(LocalizeRegion {
                    frame_id: FrameId::new("map"),
                    min_xyz_m: [0.0, 0.0, 0.0],
                    max_xyz_m: [1.0, 1.0, 1.0],
                }),
            },
            inline_correction_available: false,
            correction_fetch_required: false,
        }
    }

    const fn initial_map_revision() -> MapRevisionId {
        MapRevisionId {
            epoch: INITIAL_MAP_EPOCH,
            sequence: 0,
        }
    }

    const fn localize_revision_id(epoch: u64, sequence: u64) -> LocalizationRevisionId {
        LocalizationRevisionId { epoch, sequence }
    }

    fn keyframe(keyframe_id: &str, sequence: u64) -> Keyframe {
        Keyframe {
            keyframe_id: KeyframeId::new(keyframe_id),
            revision: localize_revision_id(1, sequence),
            pose: PoseEstimate {
                frame_id: FrameId::new("map"),
                child_frame_id: FrameId::new("base_link"),
                translation_m: [sequence as f64, 0.0, 0.0],
                rotation_xyzw: [0.0, 0.0, 0.0, 1.0],
            },
            descriptors: Vec::new(),
        }
    }

    fn sensor_y_cell() -> u32 {
        GRID_HEIGHT_CELLS / 2
    }

    fn terminal_x_cell(distance_m: f64) -> u32 {
        GRID_WIDTH_CELLS / 2 + (distance_m / GRID_RESOLUTION_M) as u32
    }

    fn index(x_cell: u32, y_cell: u32) -> usize {
        (y_cell * GRID_WIDTH_CELLS + x_cell) as usize
    }

    fn submap_response_for(
        request: &SubmapRequest,
        store: &RevisionStore,
        submaps: &SubmapStore,
        occupancy: Option<&OccupancyGrid>,
        frame_id: &FrameId,
    ) -> SubmapResponse {
        submap_response(request, store, submaps, occupancy, frame_id)
    }
}
