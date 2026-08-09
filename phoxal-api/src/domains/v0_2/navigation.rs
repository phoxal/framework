//! v0.2 navigation payloads.
#![allow(legacy_derive_helpers)]

use super::validation::{
    MAX_PATH_POSES, finite, finite_f32, optional_canonical_yaw, valid_request_id,
};
pub use crate::domains::v0_1::navigation::{FailureReason, Outcome, Request, RequestKind};

/// A bounded caller-chosen request identity. The wire representation remains
/// the historic `{ "value": "..." }` object, but callers cannot construct an
/// invalid identity by writing the field directly.
#[derive(Eq, PartialOrd, Ord, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "RequestIdWire")]
pub struct RequestId {
    value: String,
}

impl RequestId {
    pub fn try_new(value: impl Into<String>) -> std::result::Result<Self, RequestIdError> {
        let value = value.into();
        valid_request_id(&value)
            .then_some(Self { value })
            .ok_or(RequestIdError::Invalid)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        valid_request_id(&self.value)
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestIdWire {
    value: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestIdError {
    Invalid,
}

impl std::fmt::Display for RequestIdError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("request id must be a non-empty bounded ASCII token")
    }
}
impl std::error::Error for RequestIdError {}

impl TryFrom<RequestIdWire> for RequestId {
    type Error = RequestIdError;
    fn try_from(value: RequestIdWire) -> std::result::Result<Self, Self::Error> {
        Self::try_new(value.value)
    }
}

/// A server-issued operation identity. It is scoped to the producer
/// incarnation and sequence zero is reserved as the absent value.
#[derive(Copy, Eq, Hash, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "NavigationOperationIdWire")]
pub struct NavigationOperationId {
    producer: ::phoxal_bus::ProducerId,
    sequence: u64,
}

impl NavigationOperationId {
    pub fn new(producer: ::phoxal_bus::ProducerId, sequence: u64) -> Option<Self> {
        (sequence != 0).then_some(Self { producer, sequence })
    }
    #[must_use]
    pub const fn producer(&self) -> ::phoxal_bus::ProducerId {
        self.producer
    }
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NavigationOperationIdWire {
    producer: ::phoxal_bus::ProducerId,
    sequence: u64,
}

impl TryFrom<NavigationOperationIdWire> for NavigationOperationId {
    type Error = &'static str;
    fn try_from(value: NavigationOperationIdWire) -> std::result::Result<Self, Self::Error> {
        Self::new(value.producer, value.sequence)
            .ok_or("navigation operation sequence must be nonzero")
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "PoseWire")]
pub struct Pose {
    pub x_m: f64,
    pub y_m: f64,
    pub yaw_rad: Option<f64>,
}

impl Pose {
    pub fn try_new(
        x_m: f64,
        y_m: f64,
        yaw_rad: Option<f64>,
    ) -> std::result::Result<Self, NavigationError> {
        (finite(x_m) && finite(y_m) && optional_canonical_yaw(yaw_rad))
            .then_some(Self { x_m, y_m, yaw_rad })
            .ok_or(NavigationError::InvalidPose)
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PoseWire {
    x_m: f64,
    y_m: f64,
    yaw_rad: Option<f64>,
}
impl TryFrom<PoseWire> for Pose {
    type Error = NavigationError;
    fn try_from(value: PoseWire) -> std::result::Result<Self, Self::Error> {
        Self::try_new(value.x_m, value.y_m, value.yaw_rad)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "PathWire")]
pub struct Path {
    pub poses: Vec<Pose>,
    pub map_revision: Option<u64>,
}

impl Path {
    pub fn try_new(
        poses: Vec<Pose>,
        map_revision: Option<u64>,
    ) -> std::result::Result<Self, NavigationError> {
        (!poses.is_empty() && poses.len() <= MAX_PATH_POSES)
            .then_some(Self {
                poses,
                map_revision,
            })
            .ok_or(NavigationError::PathBoundExceeded)
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PathWire {
    poses: Vec<Pose>,
    map_revision: Option<u64>,
}
impl TryFrom<PathWire> for Path {
    type Error = NavigationError;
    fn try_from(value: PathWire) -> std::result::Result<Self, Self::Error> {
        Self::try_new(value.poses, value.map_revision)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StartKind {
    GotoPose(Pose),
    FollowPath(Path),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StartRequest {
    pub request_id: RequestId,
    pub kind: StartKind,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StartResponse {
    Accepted { operation_id: NavigationOperationId },
    Refused(RefusalReason),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CancelRequest {
    pub operation_id: NavigationOperationId,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CancelResponse {
    Accepted,
    Refused(RefusalReason),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    Busy,
    InvalidRequest,
    Unsupported,
    Unavailable,
    NotOwner,
    NotFound,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum State {
    Idle,
    Accepted(NavigationOperationId),
    Running(NavigationOperationId),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "ProgressWire")]
pub struct Progress {
    pub operation_id: NavigationOperationId,
    pub request_id: RequestId,
    pub distance_remaining_m: f64,
    pub path_index: u32,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressWire {
    operation_id: NavigationOperationId,
    request_id: RequestId,
    distance_remaining_m: f64,
    path_index: u32,
}
impl TryFrom<ProgressWire> for Progress {
    type Error = NavigationError;
    fn try_from(value: ProgressWire) -> std::result::Result<Self, Self::Error> {
        (finite(value.distance_remaining_m)
            && usize::try_from(value.path_index).is_ok_and(|n| n < MAX_PATH_POSES))
        .then_some(Self {
            operation_id: value.operation_id,
            request_id: value.request_id,
            distance_remaining_m: value.distance_remaining_m,
            path_index: value.path_index,
        })
        .ok_or(NavigationError::InvalidProgress)
    }
}

impl Progress {
    pub fn try_new(
        operation_id: NavigationOperationId,
        request_id: RequestId,
        distance_remaining_m: f64,
        path_index: u32,
    ) -> std::result::Result<Self, NavigationError> {
        (finite(distance_remaining_m)
            && usize::try_from(path_index).is_ok_and(|n| n < MAX_PATH_POSES))
        .then_some(Self {
            operation_id,
            request_id,
            distance_remaining_m,
            path_index,
        })
        .ok_or(NavigationError::InvalidProgress)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "ResultWire")]
pub struct Result {
    pub operation_id: NavigationOperationId,
    pub request_id: RequestId,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultWire {
    operation_id: NavigationOperationId,
    request_id: RequestId,
    outcome: Outcome,
}
impl TryFrom<ResultWire> for Result {
    type Error = NavigationError;
    fn try_from(value: ResultWire) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            operation_id: value.operation_id,
            request_id: value.request_id,
            outcome: value.outcome,
        })
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "CandidateWire")]
pub struct Candidate {
    pub operation_id: NavigationOperationId,
    pub linear_x_mps: f32,
    pub angular_z_radps: f32,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateWire {
    operation_id: NavigationOperationId,
    linear_x_mps: f32,
    angular_z_radps: f32,
}
impl TryFrom<CandidateWire> for Candidate {
    type Error = NavigationError;
    fn try_from(value: CandidateWire) -> std::result::Result<Self, Self::Error> {
        (finite_f32(value.linear_x_mps) && finite_f32(value.angular_z_radps))
            .then_some(Self {
                operation_id: value.operation_id,
                linear_x_mps: value.linear_x_mps,
                angular_z_radps: value.angular_z_radps,
            })
            .ok_or(NavigationError::InvalidCandidate)
    }
}

impl Candidate {
    pub fn try_new(
        operation_id: NavigationOperationId,
        linear_x_mps: f32,
        angular_z_radps: f32,
    ) -> std::result::Result<Self, NavigationError> {
        (finite_f32(linear_x_mps) && finite_f32(angular_z_radps))
            .then_some(Self {
                operation_id,
                linear_x_mps,
                angular_z_radps,
            })
            .ok_or(NavigationError::InvalidCandidate)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FrontierRequest {
    pub map_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "FrontierWire")]
pub struct Frontier {
    pub x_m: f64,
    pub y_m: f64,
    pub score: f32,
    pub size: u32,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontierWire {
    x_m: f64,
    y_m: f64,
    score: f32,
    size: u32,
}
impl TryFrom<FrontierWire> for Frontier {
    type Error = NavigationError;
    fn try_from(value: FrontierWire) -> std::result::Result<Self, Self::Error> {
        (finite(value.x_m)
            && finite(value.y_m)
            && finite_f32(value.score)
            && (0.0..=1.0).contains(&value.score)
            && value.size != 0)
            .then_some(Self {
                x_m: value.x_m,
                y_m: value.y_m,
                score: value.score,
                size: value.size,
            })
            .ok_or(NavigationError::InvalidFrontier)
    }
}

impl Frontier {
    pub fn try_new(
        x_m: f64,
        y_m: f64,
        score: f32,
        size: u32,
    ) -> std::result::Result<Self, NavigationError> {
        (finite(x_m)
            && finite(y_m)
            && finite_f32(score)
            && (0.0..=1.0).contains(&score)
            && size != 0)
            .then_some(Self {
                x_m,
                y_m,
                score,
                size,
            })
            .ok_or(NavigationError::InvalidFrontier)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FrontierResponse {
    pub frontier: Option<Frontier>,
    pub map_revision: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationError {
    InvalidPose,
    PathBoundExceeded,
    InvalidProgress,
    InvalidCandidate,
    InvalidFrontier,
}
impl std::fmt::Display for NavigationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidPose => "navigation pose must contain finite values and canonical yaw",
            Self::PathBoundExceeded => "navigation path must contain between one and 4096 poses",
            Self::InvalidProgress => "navigation progress must be finite and bounded",
            Self::InvalidCandidate => "navigation candidate speeds must be finite",
            Self::InvalidFrontier => "navigation frontier must be finite, bounded, and non-empty",
        })
    }
}
impl std::error::Error for NavigationError {}
