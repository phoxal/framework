//! v0.2 perception payloads.
#![allow(legacy_derive_helpers)]

#[allow(unused_imports)]
pub use crate::domains::source::{InvalidSourceRef, SourceRef};

/// A current-revision detection with wire-level finite and fixed
/// shape guarantees. The v0.1 body above remains untouched.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detection {
    pub class_id: String,
    #[serde(
        deserialize_with = "crate::domains::v0_2::perception::deserialize_finite_detection_confidence"
    )]
    pub confidence: f32,
    #[serde(
        deserialize_with = "crate::domains::v0_2::perception::deserialize_finite_detection_position"
    )]
    pub position_m: [f64; 3],
    pub frame_id: String,
    pub track_id: Option<u64>,
}

/// One source-captured perception batch. `captured_at` is copied
/// from the selected camera measurement's provenance; it is not
/// the perception step's publication instant.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detections {
    pub source: SourceRef,
    pub captured_at: ::phoxal_bus::TimeWindow,
    pub detections: Vec<Detection>,
}

/// Why the perception participant cannot provide a healthy batch.
#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthReason {
    MissingCamera,
    StaleCamera,
    InvalidCamera,
    DetectorFailure,
    BackendUnavailable,
    PublicationFailure,
    ManagedInputFailure,
}

/// The perception participant's exclusive published health.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub enum State {
    Healthy {
        detector: String,
    },
    Unhealthy {
        detector: String,
        reason: HealthReason,
    },
}

/// Reject a non-finite confidence score on the current perception wire.
pub(crate) fn deserialize_finite_detection_confidence<'de, D>(
    deserializer: D,
) -> Result<f32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <f32 as serde::Deserialize>::deserialize(deserializer)?;
    value
        .is_finite()
        .then_some(value)
        .ok_or_else(|| serde::de::Error::custom("perception confidence must be finite"))
}

/// Reject a non-finite coordinate in the current perception wire position.
///
/// Deserializing into `[f64; 3]` also enforces the fixed three-coordinate
/// shape, so malformed vectors cannot enter the detector/tracker boundary.
pub(crate) fn deserialize_finite_detection_position<'de, D>(
    deserializer: D,
) -> Result<[f64; 3], D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <[f64; 3] as serde::Deserialize>::deserialize(deserializer)?;
    value
        .iter()
        .all(|coordinate| coordinate.is_finite())
        .then_some(value)
        .ok_or_else(|| serde::de::Error::custom("perception position must be finite"))
}
