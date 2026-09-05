//! Ordered attachment state plus the race-closing current query.

crate::endpoints! {
    self: Stream<SimulationAttachmentStream, Out>;
    current: Query<CurrentRequest, CurrentResponse>;
}

use super::SimulationAttachmentState;

/// One complete replacement of the execution's attachment state.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct SimulationAttachmentStream {
    /// The current attachment, or `None` after a completed removal.
    pub attachment: Option<SimulationAttachmentState>,
}

/// Ask for the current attachment after subscribing to the ordered stream.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct CurrentRequest {}

/// The current complete attachment state.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct CurrentResponse {
    pub attachment: Option<SimulationAttachmentState>,
}
