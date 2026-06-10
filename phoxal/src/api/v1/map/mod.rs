pub const SCHEMA_NAME: &str = "phoxal-api-map/v1";
pub const SCHEMA_VERSION: u32 = 1;

use std::fmt;

use crate::api::v1::frame::FrameId;
use crate::api::v1::localize::LocalizationRevisionId;
use crate::bus::zenoh::BusyResponse;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalCost {
    pub map_revision: MapRevisionId,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub frame_id: FrameId,
    pub grid: Grid<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Traversability {
    pub map_revision: MapRevisionId,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub frame_id: FrameId,
    pub cells: Grid<TraversabilityCell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraversabilitySummary {
    pub map_revision: MapRevisionId,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub frame_id: FrameId,
    pub region: RegionSummary,
    pub status: TraversabilityStatus,
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

        impl BusyResponse for $name {
            fn busy() -> Self {
                Self(MapTileResponse::Busy)
            }
        }
    };
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SubmapRequest(pub MapTileRequest);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EsdfTileRequest(pub MapTileRequest);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TraversabilityTileRequest(pub MapTileRequest);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LocalGridRequest(pub MapTileRequest);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GlobalGridRequest(pub MapTileRequest);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SnapshotRequest(pub MapTileRequest);

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

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::BusyResponse;

    use super::{
        EsdfTile, EsdfTileResponse, GlobalGrid, GlobalGridResponse, LocalGrid, LocalGridResponse,
        MapTileResponse, Snapshot, SnapshotResponse, Submap, SubmapResponse, TraversabilityTile,
        TraversabilityTileResponse,
    };

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
