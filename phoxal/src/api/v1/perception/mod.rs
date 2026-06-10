pub const SCHEMA_NAME: &str = "phoxal-api-perception/v1";
pub const SCHEMA_VERSION: u32 = 1;

use std::fmt;

use crate::api::v1::frame::FrameId;
use crate::api::v1::localize::LocalizationRevisionId;
use crate::api::v1::map::MapRevisionId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BoundingBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    pub class_label: String,
    pub class_id: u32,
    pub confidence: f32,
    pub bbox: BoundingBox,
    pub anchor_3d_m: Option<[f64; 3]>,
    pub source_frame_id: FrameId,
    pub tracker_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionLinkage {
    pub localize_revision: LocalizationRevisionId,
    pub map_revision: MapRevisionId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detections {
    pub detections: Vec<Detection>,
    pub localize_revision: LocalizationRevisionId,
    pub map_revision: MapRevisionId,
    pub detector_id: String,
}

impl Detections {
    #[must_use]
    pub const fn revision_linkage(&self) -> RevisionLinkage {
        RevisionLinkage {
            localize_revision: self.localize_revision,
            map_revision: self.map_revision,
        }
    }

    pub fn validate_revision_linkage(
        &self,
        expected: RevisionLinkage,
    ) -> Result<RevisionLinkage, RevisionMismatch> {
        let actual = self.revision_linkage();
        if actual == expected {
            Ok(actual)
        } else {
            Err(RevisionMismatch { expected, actual })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionMismatch {
    pub expected: RevisionLinkage,
    pub actual: RevisionLinkage,
}

impl fmt::Display for RevisionMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "perception detection revisions mismatch: expected localize {:?} map {:?}, got localize {:?} map {:?}",
            self.expected.localize_revision,
            self.expected.map_revision,
            self.actual.localize_revision,
            self.actual.map_revision
        )
    }
}

impl std::error::Error for RevisionMismatch {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerceptionState {
    pub health: DetectorHealth,
    pub backend: String,
    pub model_id: String,
    pub weights_version: String,
    pub inference_budget_headroom: f32,
    pub cadence_hz: f32,
    pub dropped_frames: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectorHealth {
    Healthy,
    Degraded(PerceptionDegradedReason),
    Stopped(PerceptionStoppedReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerceptionDegradedReason {
    InferenceBudgetExceeded,
    SourceStale,
    LocalizationDegraded,
    BackendThrottled,
    ConfidenceCollapse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerceptionStoppedReason {
    ModelLoadFailed,
    ComputeUnavailable,
    SourceUnavailable,
    SupervisorStopped,
    BackendError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackedObservation {
    pub tracker_id: u64,
    pub last_seen_ns: u64,
    pub localize_revision: LocalizationRevisionId,
    pub map_revision: MapRevisionId,
    pub anchor_3d_m: Option<[f64; 3]>,
    pub source_frame_id: FrameId,
}

#[cfg(test)]
mod tests {
    use crate::api::v1::frame::FrameId;
    use crate::api::v1::localize::LocalizationRevisionId;
    use crate::api::v1::map::MapRevisionId;

    use super::{BoundingBox, Detection, Detections, RevisionLinkage};

    #[test]
    fn rejects_mismatched_revision_linkage() {
        let detections = detections(localize(7, 9), map(3, 5));
        let mismatch = detections
            .validate_revision_linkage(RevisionLinkage {
                localize_revision: localize(7, 10),
                map_revision: map(3, 5),
            })
            .expect_err("mismatched localize revision should be rejected");

        assert_eq!(mismatch.actual.localize_revision, localize(7, 9));
        assert_eq!(mismatch.actual.map_revision, map(3, 5));
    }

    #[test]
    fn round_trips_revision_linkage_through_detection_batch() {
        let detections = detections(localize(2, 4), map(9, 10));
        let encoded = serde_json::to_string(&detections).expect("serialize detections");
        let decoded: Detections = serde_json::from_str(&encoded).expect("deserialize detections");

        assert_eq!(
            decoded
                .validate_revision_linkage(RevisionLinkage {
                    localize_revision: localize(2, 4),
                    map_revision: map(9, 10),
                })
                .expect("matching revisions"),
            RevisionLinkage {
                localize_revision: localize(2, 4),
                map_revision: map(9, 10),
            }
        );
    }

    fn detections(
        localize_revision: LocalizationRevisionId,
        map_revision: MapRevisionId,
    ) -> Detections {
        Detections {
            detections: vec![Detection {
                class_label: "crate".to_string(),
                class_id: 12,
                confidence: 0.75,
                bbox: BoundingBox {
                    x: 10.0,
                    y: 20.0,
                    width: 30.0,
                    height: 40.0,
                },
                anchor_3d_m: Some([1.0, 2.0, 3.0]),
                source_frame_id: FrameId::new("front_camera__camera_link"),
                tracker_id: None,
            }],
            localize_revision,
            map_revision,
            detector_id: "placeholder".to_string(),
        }
    }

    const fn localize(epoch: u64, sequence: u64) -> LocalizationRevisionId {
        LocalizationRevisionId { epoch, sequence }
    }

    const fn map(epoch: u64, sequence: u64) -> MapRevisionId {
        MapRevisionId { epoch, sequence }
    }
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-perception/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
