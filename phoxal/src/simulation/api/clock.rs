//! The authoritative completed-world-step hand.

crate::endpoints! {
    self: WorldClock<Clock>;
}

/// The body carried by the authoritative simulation-clock hand.
///
/// The exact [`crate::bus::RobotInstant`] is in message metadata. This small
/// body carries only the simulator's monotonically increasing world-step
/// counter for diagnostics and loss detection.
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
pub struct Clock {
    pub step: u64,
}
