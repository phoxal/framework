use crate::api::__contracts::phoxal::kinematics::v1::OdometryState;
use crate::api::motion::v1::MotionConstraints;
use crate::api::motion::v1::{
    ApplyEmergencyResponse, ArmRequest, MotionIntent, ReleaseEmergencyRequest, motion,
};
use phoxal::contract::Empty;
use phoxal::runtime::input::{Commands, Latest, Setpoint};

/// One immutable input cut for Motion.
#[phoxal::runtime::inputs]
pub struct MotionInputs {
    /// Manual and autonomous control intents replace older values and expire
    /// independently from their publication timestamps.
    #[phoxal::runtime::input(port = motion::methods::MANUAL.__setpoint_port(), max_bytes = 4096)]
    pub manual: Setpoint<MotionIntent>,
    #[phoxal::runtime::input(port = motion::methods::AUTONOMOUS.__setpoint_port(), max_bytes = 4096)]
    pub autonomous: Setpoint<MotionIntent>,
    /// Safety is an expiring protective constraint product, never an
    /// authority lease or a motion-owned duplicate state.
    pub safety: Latest<MotionConstraints>,
    /// Measured evidence is required before arm and actuation.
    pub measurements: Latest<OdometryState>,
    /// Motion authority and emergency calls are merged in one service order.
    #[phoxal::runtime::input(
        port = motion::methods::ARM.__commands_port(),
        max_items = 32,
        max_bytes = 16_384
    )]
    pub arm: Commands<ArmRequest, ApplyEmergencyResponse>,
    #[phoxal::runtime::input(
        port = motion::methods::DISARM.__commands_port(),
        max_items = 32,
        max_bytes = 16_384
    )]
    pub disarm: Commands<Empty, ApplyEmergencyResponse>,
    #[phoxal::runtime::input(
        port = motion::methods::ENGAGE_EMERGENCY.__commands_port(),
        max_items = 32,
        max_bytes = 16_384
    )]
    pub engage_emergency: Commands<Empty, ApplyEmergencyResponse>,
    #[phoxal::runtime::input(
        port = motion::methods::RELEASE_EMERGENCY.__commands_port(),
        max_items = 32,
        max_bytes = 16_384
    )]
    pub release_emergency: Commands<ReleaseEmergencyRequest, ApplyEmergencyResponse>,
}
