//! End the attachment from its source-bound world host.

crate::endpoints! {
    self: Query<EndRequest, EndResponse>;
}

use super::{SimulationAttachmentState, SimulationEndReason};

/// One typed terminal outcome reported by the bound host.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct EndRequest {
    pub reason: SimulationEndReason,
}

/// The Removing state accepted from the bound host.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct EndResponse {
    pub attachment: SimulationAttachmentState,
}
