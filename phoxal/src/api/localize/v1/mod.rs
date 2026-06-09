pub const SCHEMA_NAME: &str = "phoxal-api-localize/v1";
pub const SCHEMA_VERSION: u32 = 1;

use std::fmt;

use crate::api::frame::v1::FrameId;
use crate::bus::zenoh::{BusyResponse, TypedSchema};
use serde::{Deserialize, Serialize};

pub mod stream_demand;

pub use stream_demand::LocalizeStreamDemands;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizationRevisionId {
    pub epoch: u64,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct KeyframeId(pub String);

impl KeyframeId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for KeyframeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalizationState {
    pub mode: LocalizationMode,
    pub source: LocalizationSource,
    pub revision: Option<LocalizationRevisionId>,
    pub pose: Option<PoseEstimate>,
    pub velocity: Option<VelocityEstimate>,
    pub covariance: Option<Covariance>,
    pub imu_bias: Option<ImuBiasEstimate>,
    pub status: LocalizationStatus,
    pub valid_at_ns: Option<u64>,
}

impl TypedSchema for LocalizationState {
    const SCHEMA_NAME: &'static str = "runtime/localize/state";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum LocalizationMode {
    Initializing,
    DeadReckoning,
    Tracking,
    Relocalizing,
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LocalizationSource {
    OrbSlam3Rgbd,
    OrbSlam3RgbdInertial,
    DeadReckoning,
    SimulatorTruth,
    GnssAnchored,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoseEstimate {
    pub frame_id: FrameId,
    pub child_frame_id: FrameId,
    pub translation_m: [f64; 3],
    pub rotation_xyzw: [f64; 4],
}

impl TypedSchema for PoseEstimate {
    const SCHEMA_NAME: &'static str = "runtime/localize/pose";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VelocityEstimate {
    pub frame_id: FrameId,
    pub linear_mps: [f64; 3],
    pub angular_radps: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Covariance {
    pub values: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImuBiasEstimate {
    pub accel_bias_mps2: [f64; 3],
    pub gyro_bias_radps: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalizationStatus {
    pub healthy: bool,
    pub reasons: Vec<LocalizationStatusReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum LocalizationStatusReason {
    /// A required localization sensor stream has not produced usable input.
    SensorMissing,
    /// A required localization sensor stream is outside its freshness window.
    SensorStale,
    /// The selected backend is alive but has not produced a usable estimate yet.
    BackendInitializing,
    /// The selected backend reported an internal failure.
    BackendError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalizationRevision {
    pub revision_id: LocalizationRevisionId,
    pub previous_revision_id: Option<LocalizationRevisionId>,
    pub cause: LocalizationRevisionCause,
    pub affected_keyframes: AffectedKeyframeSummary,
    pub inline_correction_available: bool,
    pub correction_fetch_required: bool,
}

impl TypedSchema for LocalizationRevision {
    const SCHEMA_NAME: &'static str = "runtime/localize/revision";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum LocalizationRevisionCause {
    SensorIntegration,
    LoopClosure,
    Relocalization,
    Reset,
    BackendRecovery,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AffectedKeyframeSummary {
    pub keyframe_ids: Vec<KeyframeId>,
    pub region: Option<Region>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub keyframe_id: KeyframeId,
    pub revision: LocalizationRevisionId,
    pub pose: PoseEstimate,
    pub descriptors: Vec<KeyframeDescriptor>,
}

impl TypedSchema for Keyframe {
    const SCHEMA_NAME: &'static str = "runtime/localize/keyframe";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyframeDescriptor {
    pub kind: KeyframeDescriptorKind,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum KeyframeDescriptorKind {
    OrbFeatures,
    BowVector,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoseGraphCorrection {
    pub from_revision: LocalizationRevisionId,
    pub to_revision: LocalizationRevisionId,
    pub affected_keyframes: Vec<KeyframeId>,
    pub transforms: Vec<CorrectionTransform>,
}

impl TypedSchema for PoseGraphCorrection {
    const SCHEMA_NAME: &'static str = "runtime/localize/correction";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorrectionTransform {
    pub frame_id: FrameId,
    pub child_frame_id: FrameId,
    pub translation_m: [f64; 3],
    pub rotation_xyzw: [f64; 4],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorrectionsRequest {
    pub from_revision: LocalizationRevisionId,
    pub to_revision: LocalizationRevisionId,
    pub max_bytes: Option<u32>,
}

impl TypedSchema for CorrectionsRequest {
    const SCHEMA_NAME: &'static str = "runtime/localize/query/corrections/request";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CorrectionsResponse {
    Ok {
        from_revision: LocalizationRevisionId,
        to_revision: LocalizationRevisionId,
        corrections: Vec<PoseGraphCorrection>,
    },
    WrongEpoch {
        current: LocalizationRevisionId,
    },
    StaleRevision {
        current: LocalizationRevisionId,
    },
    RevisionUnavailable {
        latest_available: Option<LocalizationRevisionId>,
    },
    ResponseTooLarge {
        available_bytes: u64,
    },
    Busy,
}

impl TypedSchema for CorrectionsResponse {
    const SCHEMA_NAME: &'static str = "runtime/localize/query/corrections/response";
    const SCHEMA_VERSION: u32 = 1;
}

impl BusyResponse for CorrectionsResponse {
    fn busy() -> Self {
        Self::Busy
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoseGraphRequest {
    pub revision: LocalizationRevisionId,
    pub range: PoseGraphRange,
    pub max_bytes: Option<u32>,
}

impl TypedSchema for PoseGraphRequest {
    const SCHEMA_NAME: &'static str = "runtime/localize/query/pose_graph/request";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PoseGraphResponse {
    Ok {
        served_revision: LocalizationRevisionId,
        graph: PoseGraphSnapshot,
    },
    WrongEpoch {
        current: LocalizationRevisionId,
    },
    StaleRevision {
        current: LocalizationRevisionId,
    },
    RevisionUnavailable {
        latest_available: Option<LocalizationRevisionId>,
    },
    ResponseTooLarge {
        available_bytes: u64,
    },
    Busy,
}

impl TypedSchema for PoseGraphResponse {
    const SCHEMA_NAME: &'static str = "runtime/localize/query/pose_graph/response";
    const SCHEMA_VERSION: u32 = 1;
}

impl BusyResponse for PoseGraphResponse {
    fn busy() -> Self {
        Self::Busy
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PoseGraphRange {
    All,
    Keyframes(Vec<KeyframeId>),
    Region(Region),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoseGraphSnapshot {
    pub revision: LocalizationRevisionId,
    pub keyframes: Vec<Keyframe>,
    pub edges: Vec<PoseGraphEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoseGraphEdge {
    pub from_keyframe_id: KeyframeId,
    pub to_keyframe_id: KeyframeId,
    pub transform: CorrectionTransform,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyframeRequest {
    pub revision: LocalizationRevisionId,
    pub keyframe_id: KeyframeId,
    pub max_bytes: Option<u32>,
}

impl TypedSchema for KeyframeRequest {
    const SCHEMA_NAME: &'static str = "runtime/localize/query/keyframe/request";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum KeyframeResponse {
    Ok {
        served_revision: LocalizationRevisionId,
        keyframe: Keyframe,
    },
    WrongEpoch {
        current: LocalizationRevisionId,
    },
    StaleRevision {
        current: LocalizationRevisionId,
    },
    RevisionUnavailable {
        latest_available: Option<LocalizationRevisionId>,
    },
    UnknownKeyframe {
        keyframe_id: KeyframeId,
    },
    ResponseTooLarge {
        available_bytes: u64,
    },
    Busy,
}

impl TypedSchema for KeyframeResponse {
    const SCHEMA_NAME: &'static str = "runtime/localize/query/keyframe/response";
    const SCHEMA_VERSION: u32 = 1;
}

impl BusyResponse for KeyframeResponse {
    fn busy() -> Self {
        Self::Busy
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub frame_id: FrameId,
    pub min_xyz_m: [f64; 3],
    pub max_xyz_m: [f64; 3],
}

crate::bus::topic_leaf! {
    pubsub state {
        path: "runtime/localize/state",
        payload: LocalizationState
    }
}

crate::bus::topic_leaf! {
    pubsub pose {
        path: "runtime/localize/pose",
        payload: PoseEstimate
    }
}

crate::bus::topic_leaf! {
    pubsub revision {
        path: "runtime/localize/revision",
        payload: LocalizationRevision
    }
}

crate::bus::topic_leaf! {
    pubsub keyframe {
        path: "runtime/localize/keyframe",
        payload: Keyframe
    }
}

crate::bus::topic_leaf! {
    pubsub correction {
        path: "runtime/localize/correction",
        payload: PoseGraphCorrection
    }
}

pub mod query {
    use super::*;

    crate::bus::topic_leaf! {
        query pose_graph {
            path: "runtime/localize/query/pose_graph",
            request: PoseGraphRequest,
            response: PoseGraphResponse
        }
    }

    crate::bus::topic_leaf! {
        query keyframe {
            path: "runtime/localize/query/keyframe",
            request: KeyframeRequest,
            response: KeyframeResponse
        }
    }

    crate::bus::topic_leaf! {
        query corrections {
            path: "runtime/localize/query/corrections",
            request: CorrectionsRequest,
            response: CorrectionsResponse
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::{BusyResponse, TypedSchema};

    use crate::api::localize::v1::{
        CorrectionsRequest, CorrectionsResponse, Keyframe, KeyframeRequest, KeyframeResponse,
        LocalizationRevision, LocalizationSource, LocalizationState, PoseEstimate,
        PoseGraphCorrection, PoseGraphRequest, PoseGraphResponse,
    };

    #[test]
    fn localization_contract_schemas_are_stable() {
        assert_eq!(LocalizationState::SCHEMA_NAME, "runtime/localize/state");
        assert_eq!(LocalizationState::SCHEMA_VERSION, 1);
        assert_eq!(PoseEstimate::SCHEMA_NAME, "runtime/localize/pose");
        assert_eq!(PoseEstimate::SCHEMA_VERSION, 1);
        assert_eq!(
            LocalizationRevision::SCHEMA_NAME,
            "runtime/localize/revision"
        );
        assert_eq!(LocalizationRevision::SCHEMA_VERSION, 1);
        assert_eq!(Keyframe::SCHEMA_NAME, "runtime/localize/keyframe");
        assert_eq!(Keyframe::SCHEMA_VERSION, 1);
        assert_eq!(
            PoseGraphCorrection::SCHEMA_NAME,
            "runtime/localize/correction"
        );
        assert_eq!(PoseGraphCorrection::SCHEMA_VERSION, 1);
        assert_eq!(
            CorrectionsRequest::SCHEMA_NAME,
            "runtime/localize/query/corrections/request"
        );
        assert_eq!(CorrectionsRequest::SCHEMA_VERSION, 1);
        assert_eq!(
            CorrectionsResponse::SCHEMA_NAME,
            "runtime/localize/query/corrections/response"
        );
        assert_eq!(CorrectionsResponse::SCHEMA_VERSION, 1);
        assert_eq!(
            PoseGraphRequest::SCHEMA_NAME,
            "runtime/localize/query/pose_graph/request"
        );
        assert_eq!(PoseGraphRequest::SCHEMA_VERSION, 1);
        assert_eq!(
            PoseGraphResponse::SCHEMA_NAME,
            "runtime/localize/query/pose_graph/response"
        );
        assert_eq!(PoseGraphResponse::SCHEMA_VERSION, 1);
        assert_eq!(
            KeyframeRequest::SCHEMA_NAME,
            "runtime/localize/query/keyframe/request"
        );
        assert_eq!(KeyframeRequest::SCHEMA_VERSION, 1);
        assert_eq!(
            KeyframeResponse::SCHEMA_NAME,
            "runtime/localize/query/keyframe/response"
        );
        assert_eq!(KeyframeResponse::SCHEMA_VERSION, 1);
    }

    #[test]
    fn localization_source_serializes_as_contract_snake_case() {
        let value =
            serde_json::to_string(&LocalizationSource::SimulatorTruth).expect("source serializes");
        let gnss_value =
            serde_json::to_string(&LocalizationSource::GnssAnchored).expect("source serializes");

        assert_eq!(value, "\"simulator_truth\"");
        assert_eq!(gnss_value, "\"gnss_anchored\"");
    }

    #[test]
    fn localize_query_responses_have_busy_values() {
        assert_eq!(PoseGraphResponse::busy(), PoseGraphResponse::Busy);
        assert_eq!(KeyframeResponse::busy(), KeyframeResponse::Busy);
        assert_eq!(CorrectionsResponse::busy(), CorrectionsResponse::Busy);
    }

    #[test]
    fn topic_paths_are_stable() {
        assert_eq!(super::state::path(), "runtime/localize/state");
        assert_eq!(super::pose::path(), "runtime/localize/pose");
        assert_eq!(super::revision::path(), "runtime/localize/revision");
        assert_eq!(super::keyframe::path(), "runtime/localize/keyframe");
        assert_eq!(super::correction::path(), "runtime/localize/correction");
        assert_eq!(
            super::query::pose_graph::path(),
            "runtime/localize/query/pose_graph"
        );
        assert_eq!(
            super::query::keyframe::path(),
            "runtime/localize/query/keyframe"
        );
        assert_eq!(
            super::query::corrections::path(),
            "runtime/localize/query/corrections"
        );
    }
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-localize/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
