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
#[serde(deny_unknown_fields)]
pub struct WorldSessionControlRequest {
    /// The world instance the operation must target before it is dispatched.
    pub instance: WorldInstanceId,
    /// The idempotent world motion operation to apply.
    pub operation: WorldControl,
}

/// One idempotent world motion operation.
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
pub enum WorldControl {
    Pause,
    Resume,
    Stop,
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldSessionControlResponse {
    pub state: WorldSessionState,
}
