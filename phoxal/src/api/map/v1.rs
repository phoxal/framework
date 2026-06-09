pub const SCHEMA_NAME: &str = "phoxal-api-map/v1";
pub const SCHEMA_VERSION: u32 = 1;

use std::fmt;

use crate::api::frame::v1::FrameId;
use crate::api::localize::v1::LocalizationRevisionId;
use crate::bus::zenoh::{BusyResponse, TypedSchema};
use serde::{Deserialize, Serialize};

pub const REVISION_TOPIC: &str = "runtime/map/revision";
pub const SUMMARY_TOPIC: &str = "runtime/map/summary";
pub const LOCAL_COST_TOPIC: &str = "runtime/map/local_cost";
pub const TRAVERSABILITY_TOPIC: &str = "runtime/map/traversability";
pub const TRAVERSABILITY_SUMMARY_TOPIC: &str = "runtime/map/traversability_summary";
pub const QUERY_SUBMAP_TOPIC: &str = "runtime/map/query/submap";
pub const QUERY_ESDF_TILE_TOPIC: &str = "runtime/map/query/esdf_tile";
pub const QUERY_TRAVERSABILITY_TILE_TOPIC: &str = "runtime/map/query/traversability_tile";
pub const QUERY_LOCAL_GRID_TOPIC: &str = "runtime/map/query/local_grid";
pub const QUERY_GLOBAL_GRID_TOPIC: &str = "runtime/map/query/global_grid";
pub const QUERY_SNAPSHOT_TOPIC: &str = "runtime/map/query/snapshot";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapRevisionId {
    pub epoch: u64,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SubmapId(pub String);

impl SubmapId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for SubmapId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapRevision {
    pub map_revision_id: MapRevisionId,
    pub previous_map_revision_id: Option<MapRevisionId>,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub cause: MapRevisionCause,
    pub affected_region: Option<RegionSummary>,
}

impl TypedSchema for MapRevision {
    const SCHEMA_NAME: &'static str = "runtime/map/revision";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MapRevisionCause {
    SensorIntegration,
    LocalizationCorrection,
    SubmapFinalized,
    Import,
    Reset,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionSummary {
    pub frame_id: FrameId,
    pub min_xyz_m: [f64; 3],
    pub max_xyz_m: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub current_revision: Option<MapRevisionId>,
    pub built_from_localize_revision: Option<LocalizationRevisionId>,
    pub frame_id: FrameId,
    pub known_region: Option<RegionSummary>,
}

impl TypedSchema for Summary {
    const SCHEMA_NAME: &'static str = "runtime/map/summary";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalCost {
    pub map_revision: MapRevisionId,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub frame_id: FrameId,
    pub grid: Grid<f32>,
}

impl TypedSchema for LocalCost {
    const SCHEMA_NAME: &'static str = "runtime/map/local_cost";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Traversability {
    pub map_revision: MapRevisionId,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub frame_id: FrameId,
    pub cells: Grid<TraversabilityCell>,
}

impl TypedSchema for Traversability {
    const SCHEMA_NAME: &'static str = "runtime/map/traversability";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraversabilitySummary {
    pub map_revision: MapRevisionId,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub frame_id: FrameId,
    pub region: RegionSummary,
    pub status: TraversabilityStatus,
}

impl TypedSchema for TraversabilitySummary {
    const SCHEMA_NAME: &'static str = "runtime/map/traversability_summary";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TraversabilityStatus {
    Ready,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TraversabilityCell {
    Unknown,
    Free,
    Occupied,
    Inflated,
    Cliff,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grid<T> {
    pub origin_xy_m: [f64; 2],
    pub resolution: Resolution,
    pub width_cells: u32,
    pub height_cells: u32,
    pub cells: Vec<T>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapTileRequest {
    pub requested_revision: MapRevisionId,
    pub region: Region,
    pub resolution: Resolution,
    pub frame_id: FrameId,
    pub max_bytes: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub min_xyz_m: [f64; 3],
    pub max_xyz_m: [f64; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    pub xy_m: f64,
    pub z_m: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MapTileResponse<T> {
    Ok {
        served_map_revision: MapRevisionId,
        built_from_localize_revision: LocalizationRevisionId,
        frame_id: FrameId,
        payload: T,
    },
    WrongEpoch {
        current: MapRevisionId,
    },
    StaleRevision {
        current: MapRevisionId,
    },
    RevisionUnavailable {
        latest_available: Option<MapRevisionId>,
    },
    RegionUnavailable {
        served_map_revision: MapRevisionId,
    },
    ResponseTooLarge {
        available_bytes: u64,
    },
    Busy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Submap {
    pub submap_id: SubmapId,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EsdfTile {
    pub distances_m: Grid<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraversabilityTile {
    pub cells: Grid<TraversabilityCell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalGrid {
    pub cells: Grid<OccupancyCell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GlobalGrid {
    pub cells: Grid<OccupancyCell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub map_revision: MapRevisionId,
    pub submaps: Vec<Submap>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OccupancyCell {
    Unknown,
    Free,
    Occupied,
}

macro_rules! response_schema {
    ($name:ident, $payload:ty, $schema:literal) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub MapTileResponse<$payload>);

        impl TypedSchema for $name {
            const SCHEMA_NAME: &'static str = $schema;
            const SCHEMA_VERSION: u32 = 1;
        }

        impl BusyResponse for $name {
            fn busy() -> Self {
                Self(MapTileResponse::Busy)
            }
        }
    };
}

crate::bus::request_schema!(SubmapRequest, "runtime/map/query/submap/request");
crate::bus::request_schema!(EsdfTileRequest, "runtime/map/query/esdf_tile/request");
crate::bus::request_schema!(
    TraversabilityTileRequest,
    "runtime/map/query/traversability_tile/request"
);
crate::bus::request_schema!(LocalGridRequest, "runtime/map/query/local_grid/request");
crate::bus::request_schema!(GlobalGridRequest, "runtime/map/query/global_grid/request");
crate::bus::request_schema!(SnapshotRequest, "runtime/map/query/snapshot/request");

response_schema!(SubmapResponse, Submap, "runtime/map/query/submap/response");
response_schema!(
    EsdfTileResponse,
    EsdfTile,
    "runtime/map/query/esdf_tile/response"
);
response_schema!(
    TraversabilityTileResponse,
    TraversabilityTile,
    "runtime/map/query/traversability_tile/response"
);
response_schema!(
    LocalGridResponse,
    LocalGrid,
    "runtime/map/query/local_grid/response"
);
response_schema!(
    GlobalGridResponse,
    GlobalGrid,
    "runtime/map/query/global_grid/response"
);
response_schema!(
    SnapshotResponse,
    Snapshot,
    "runtime/map/query/snapshot/response"
);

crate::bus::pubsub_leaf!(revision, REVISION_TOPIC, MapRevision);
crate::bus::pubsub_leaf!(summary, SUMMARY_TOPIC, Summary);
crate::bus::pubsub_leaf!(local_cost, LOCAL_COST_TOPIC, LocalCost);
crate::bus::pubsub_leaf!(traversability, TRAVERSABILITY_TOPIC, Traversability);
crate::bus::pubsub_leaf!(
    traversability_summary,
    TRAVERSABILITY_SUMMARY_TOPIC,
    TraversabilitySummary
);

pub mod query {
    use super::*;

    crate::bus::query_leaf!(submap, QUERY_SUBMAP_TOPIC, SubmapRequest, SubmapResponse);
    crate::bus::query_leaf!(
        esdf_tile,
        QUERY_ESDF_TILE_TOPIC,
        EsdfTileRequest,
        EsdfTileResponse
    );
    crate::bus::query_leaf!(
        traversability_tile,
        QUERY_TRAVERSABILITY_TILE_TOPIC,
        TraversabilityTileRequest,
        TraversabilityTileResponse
    );
    crate::bus::query_leaf!(
        local_grid,
        QUERY_LOCAL_GRID_TOPIC,
        LocalGridRequest,
        LocalGridResponse
    );
    crate::bus::query_leaf!(
        global_grid,
        QUERY_GLOBAL_GRID_TOPIC,
        GlobalGridRequest,
        GlobalGridResponse
    );
    crate::bus::query_leaf!(
        snapshot,
        QUERY_SNAPSHOT_TOPIC,
        SnapshotRequest,
        SnapshotResponse
    );
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::{BusyResponse, TypedSchema};

    use crate::api::map::v1::{
        EsdfTile, EsdfTileRequest, EsdfTileResponse, GlobalGrid, GlobalGridRequest,
        GlobalGridResponse, LocalCost, LocalGrid, LocalGridRequest, LocalGridResponse, MapRevision,
        MapTileResponse, Snapshot, SnapshotRequest, SnapshotResponse, Submap, SubmapRequest,
        SubmapResponse, Summary, Traversability, TraversabilitySummary, TraversabilityTile,
        TraversabilityTileRequest, TraversabilityTileResponse,
    };

    #[test]
    fn map_contract_schemas_are_stable() {
        assert_eq!(MapRevision::SCHEMA_NAME, "runtime/map/revision");
        assert_eq!(MapRevision::SCHEMA_VERSION, 1);
        assert_eq!(Summary::SCHEMA_NAME, "runtime/map/summary");
        assert_eq!(Summary::SCHEMA_VERSION, 1);
        assert_eq!(LocalCost::SCHEMA_NAME, "runtime/map/local_cost");
        assert_eq!(LocalCost::SCHEMA_VERSION, 1);
        assert_eq!(Traversability::SCHEMA_NAME, "runtime/map/traversability");
        assert_eq!(Traversability::SCHEMA_VERSION, 1);
        assert_eq!(
            TraversabilitySummary::SCHEMA_NAME,
            "runtime/map/traversability_summary"
        );
        assert_eq!(TraversabilitySummary::SCHEMA_VERSION, 1);
        assert_eq!(
            SubmapRequest::SCHEMA_NAME,
            "runtime/map/query/submap/request"
        );
        assert_eq!(SubmapRequest::SCHEMA_VERSION, 1);
        assert_eq!(
            SubmapResponse::SCHEMA_NAME,
            "runtime/map/query/submap/response"
        );
        assert_eq!(SubmapResponse::SCHEMA_VERSION, 1);
        assert_eq!(
            EsdfTileRequest::SCHEMA_NAME,
            "runtime/map/query/esdf_tile/request"
        );
        assert_eq!(EsdfTileRequest::SCHEMA_VERSION, 1);
        assert_eq!(
            EsdfTileResponse::SCHEMA_NAME,
            "runtime/map/query/esdf_tile/response"
        );
        assert_eq!(EsdfTileResponse::SCHEMA_VERSION, 1);
        assert_eq!(
            TraversabilityTileRequest::SCHEMA_NAME,
            "runtime/map/query/traversability_tile/request"
        );
        assert_eq!(TraversabilityTileRequest::SCHEMA_VERSION, 1);
        assert_eq!(
            TraversabilityTileResponse::SCHEMA_NAME,
            "runtime/map/query/traversability_tile/response"
        );
        assert_eq!(TraversabilityTileResponse::SCHEMA_VERSION, 1);
        assert_eq!(
            LocalGridRequest::SCHEMA_NAME,
            "runtime/map/query/local_grid/request"
        );
        assert_eq!(LocalGridRequest::SCHEMA_VERSION, 1);
        assert_eq!(
            LocalGridResponse::SCHEMA_NAME,
            "runtime/map/query/local_grid/response"
        );
        assert_eq!(LocalGridResponse::SCHEMA_VERSION, 1);
        assert_eq!(
            GlobalGridRequest::SCHEMA_NAME,
            "runtime/map/query/global_grid/request"
        );
        assert_eq!(GlobalGridRequest::SCHEMA_VERSION, 1);
        assert_eq!(
            GlobalGridResponse::SCHEMA_NAME,
            "runtime/map/query/global_grid/response"
        );
        assert_eq!(GlobalGridResponse::SCHEMA_VERSION, 1);
        assert_eq!(
            SnapshotRequest::SCHEMA_NAME,
            "runtime/map/query/snapshot/request"
        );
        assert_eq!(SnapshotRequest::SCHEMA_VERSION, 1);
        assert_eq!(
            SnapshotResponse::SCHEMA_NAME,
            "runtime/map/query/snapshot/response"
        );
        assert_eq!(SnapshotResponse::SCHEMA_VERSION, 1);
    }

    #[test]
    fn query_responses_report_busy() {
        assert_eq!(
            <SubmapResponse as BusyResponse>::busy(),
            SubmapResponse(MapTileResponse::<Submap>::Busy)
        );
        assert_eq!(
            <EsdfTileResponse as BusyResponse>::busy(),
            EsdfTileResponse(MapTileResponse::<EsdfTile>::Busy)
        );
        assert_eq!(
            <TraversabilityTileResponse as BusyResponse>::busy(),
            TraversabilityTileResponse(MapTileResponse::<TraversabilityTile>::Busy)
        );
        assert_eq!(
            <LocalGridResponse as BusyResponse>::busy(),
            LocalGridResponse(MapTileResponse::<LocalGrid>::Busy)
        );
        assert_eq!(
            <GlobalGridResponse as BusyResponse>::busy(),
            GlobalGridResponse(MapTileResponse::<GlobalGrid>::Busy)
        );
        assert_eq!(
            <SnapshotResponse as BusyResponse>::busy(),
            SnapshotResponse(MapTileResponse::<Snapshot>::Busy)
        );
    }
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-map/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
