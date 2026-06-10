pub const SCHEMA_NAME: &str = "phoxal-api-localize/v1";
pub const SCHEMA_VERSION: u32 = 1;

use std::fmt;

use crate::api::frame::v1::FrameId;
use crate::bus::zenoh::BusyResponse;
use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::BusyResponse;

    use super::{CorrectionsResponse, KeyframeResponse, LocalizationSource, PoseGraphResponse};

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
