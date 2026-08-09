//! v0.1 navigation payloads.
#![allow(legacy_derive_helpers)]

/// A caller-chosen identifier for one navigation request.
///
/// `Ord` is derived so a consumer can key an ordered map on the
/// identity itself rather than on a copy of its inner `String`,
/// which is what keeps the newtype meaningful past the point where
/// requests are tracked. The derives add no bytes to the wire.
#[derive(Eq, PartialOrd, Ord, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RequestId {
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pose {
    pub x_m: f64,
    pub y_m: f64,
    pub yaw_rad: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Path {
    pub poses: Vec<Pose>,
    pub map_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RequestKind {
    GotoPose(Pose),
    FollowPath(Path),
    Cancel(RequestId),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Request {
    pub request_id: RequestId,
    pub kind: RequestKind,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum State {
    Idle,
    Accepted(RequestId),
    Running(RequestId),
}

#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    LocalizationUnavailable,
    MapUnavailable,
    MapChanged,
    NoPath,
    Blocked,
    Internal,
}

#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    Busy,
    InvalidRequest,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Outcome {
    Succeeded,
    Failed(FailureReason),
    Refused(RefusalReason),
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Progress {
    pub request_id: RequestId,
    pub distance_remaining_m: f64,
    pub path_index: u32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Result {
    pub request_id: RequestId,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    pub request_id: RequestId,
    pub linear_x_mps: f32,
    pub angular_z_radps: f32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FrontierRequest {
    pub map_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Frontier {
    pub x_m: f64,
    pub y_m: f64,
    pub score: f32,
    pub size: u32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FrontierResponse {
    pub frontier: Option<Frontier>,
    pub map_revision: Option<u64>,
}
