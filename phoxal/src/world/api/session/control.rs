//! Explicit idempotent world motion and stop requests.

crate::endpoints! {
    self: Query<WorldSessionControlRequest, WorldSessionControlResponse>;
}

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

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldSessionControlResponse {
    pub state: WorldSessionState,
}
