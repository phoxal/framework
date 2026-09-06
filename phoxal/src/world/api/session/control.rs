//! Explicit idempotent world motion and stop requests.

crate::endpoints! {
    self: Query<WorldSessionControlRequest, WorldSessionControlResponse>;
}

use super::WorldInstanceId;
use super::state::WorldSessionState;

#[derive(
    phoxal_macros::DescribeWire,
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum WorldSessionControlRequest {
    Pause,
    Resume,
    Stop,
}

/// One identity-bound mutating request on a verified world-session endpoint.
///
/// The endpoint itself is reusable local infrastructure.
/// The immutable world instance is therefore checked by the host before it
/// dispatches the requested side effect.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldSessionControlEnvelope {
    pub instance: WorldInstanceId,
    pub operation: WorldSessionControlRequest,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldSessionControlResponse {
    pub state: WorldSessionState,
}
