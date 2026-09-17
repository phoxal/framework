use phoxal::runtime::input::{Commands, Latest, Setpoint};
use phoxal_service_kinematics::OdometryState;
use phoxal_service_motion::{ApplyEmergencyRequest, ApplyEmergencyResponse, MotionIntent, ports};
use phoxal_service_motion::MotionConstraints;

/// One immutable input cut for Motion.
#[phoxal::runtime::inputs]
pub struct MotionInputs {
    /// Manual and autonomous control intents replace older values and expire
    /// independently from their publication timestamps.
    pub manual: Setpoint<MotionIntent>,
    pub autonomous: Setpoint<MotionIntent>,
    /// Safety is an expiring protective constraint product, never an
    /// authority lease or a motion-owned duplicate state.
    pub safety: Latest<MotionConstraints>,
    /// Measured evidence is required before arm and actuation.
    pub measurements: Latest<OdometryState>,
    /// Emergency, arm, and disarm commands are processed in admission order.
    #[phoxal::runtime::input(
        port = ports::EMERGENCY,
        max_items = 32,
        max_bytes = 16_384
    )]
    pub emergency: Commands<ApplyEmergencyRequest, ApplyEmergencyResponse>,
}
