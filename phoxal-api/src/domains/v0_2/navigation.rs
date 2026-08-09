//! v0.2 navigation payloads.
#![allow(legacy_derive_helpers)]

#[allow(unused_imports)]
pub use crate::domains::v0_1::navigation::{FailureReason, Outcome, Path, Pose, RequestId};

/// A navigation server-issued operation identity.  The producer
/// scopes the local sequence to this service incarnation, so a
/// restart inside one execution cannot collide with an old
/// operation that used the same counter value.
#[derive(Copy, Eq, Hash, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NavigationOperationId {
    pub producer: ::phoxal_bus::ProducerId,
    pub sequence: u64,
}

/// Work accepted by the navigation server.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StartKind {
    GotoPose(Pose),
    FollowPath(Path),
}

/// A start admission request. The requester producer comes from
/// the trusted query envelope. An accepted `(requester,
/// request_id)` is idempotent while the server retains it: stock
/// navigation keeps the 1,024 most recent accepted admissions
/// globally. Refusals are current-state responses and are not
/// retained. A client must not retry an accepted request after it
/// has been evicted, or across a navigation reset/restart.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StartRequest {
    pub request_id: RequestId,
    pub kind: StartKind,
}

/// The server's idempotent admission response.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StartResponse {
    Accepted { operation_id: NavigationOperationId },
    Refused(RefusalReason),
}

/// Cancel one server-issued operation. The query requester must
/// be the operation owner. A terminal operation remains
/// idempotently cancellable only while stock navigation retains
/// it in its global 1,024-operation completion window; after
/// eviction the server reports `NotFound`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CancelRequest {
    pub operation_id: NavigationOperationId,
}

/// The cancellation admission response.
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

// Results are ordered completion events, not a latest-value
// snapshot. Keep the published v0.1 role immutable and correct the
// active revision's delivery family explicitly.

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum State {
    Idle,
    Accepted(NavigationOperationId),
    Running(NavigationOperationId),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Progress {
    pub operation_id: NavigationOperationId,
    pub request_id: RequestId,
    pub distance_remaining_m: f64,
    pub path_index: u32,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Result {
    pub operation_id: NavigationOperationId,
    pub request_id: RequestId,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    pub operation_id: NavigationOperationId,
    pub linear_x_mps: f32,
    pub angular_z_radps: f32,
}
