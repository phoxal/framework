//! Frozen host bootstrap and idempotent fresh-execution attachment.

crate::endpoints! {
    self: Query<WorldSessionConnectRequest, WorldSessionConnectResponse>;
}

use super::state::WorldSessionState;
use super::{SpawnId, WorldDigest, WorldId, WorldInstanceId};
use crate::identity::ExecutionId;
use crate::version::FrameworkVersion;

/// The immutable facts a registry lookup verifies before trusting a live host.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldSessionBootstrap {
    pub instance: WorldInstanceId,
    pub framework: FrameworkVersion,
    pub world: WorldId,
    pub digest: WorldDigest,
}

/// One request on the local session's frozen entry point.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum WorldSessionConnectRequest {
    Bootstrap { framework: FrameworkVersion },
    Attach {
        framework: FrameworkVersion,
        execution: ExecutionId,
        supervisor_endpoint: String,
        spawn: Option<SpawnId>,
    },
}

/// A bootstrap observation or the complete state after idempotent admission.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum WorldSessionConnectResponse {
    Bootstrap { bootstrap: WorldSessionBootstrap },
    Attached { state: WorldSessionState },
}
