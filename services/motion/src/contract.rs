//! The motion service's payload vocabulary and endpoint contract.
//!
//! This executable authors its `phoxal.motion.v1` intent, status, and
//! emergency-response messages, private to this binary.
//! The consumed odometry input uses the SDK robotics standard.
//! Safety constraints are a private typed expectation that a robot adapts
//! from the selected Safety provider.
//! The actuator output uses the SDK standard
//! `phoxal::contracts::component::actuator::ActuatorSetpoint`.
use phoxal::contracts::robotics::OdometryState;

use phoxal::contracts::component::actuator::ActuatorSetpoint;
use phoxal::contracts::{Empty, Latest, RequestReply};

/// Why motion is constrained.
#[phoxal::message]
pub enum ConstraintReason {
    Unspecified = 0,
    WorldUnavailable = 1,
    LocalizationUncertain = 2,
    MapUnavailable = 3,
    MapBlocked = 4,
    RangeUnavailable = 5,
    RangeFault = 6,
    ObstacleProximity = 7,
    MotionUnavailable = 8,
    MotionFault = 9,
}

/// One motion constraint.
#[phoxal::message]
pub struct Constraint {
    #[phoxal(tag = 1)]
    pub reason: ConstraintReason,
    #[phoxal(tag = 2)]
    pub max_linear_speed_mps: Option<f64>,
    #[phoxal(tag = 3)]
    pub max_angular_speed_radps: Option<f64>,
    #[phoxal(tag = 4)]
    pub observed_value: Option<f64>,
}

/// The motion permission verdict.
#[phoxal::message]
pub enum Permission {
    Unspecified = 0,
    Clear = 1,
    Limited = 2,
    Stopped = 3,
}

/// The complete motion constraint product.
#[phoxal::message]
pub struct MotionConstraints {
    #[phoxal(tag = 1)]
    pub sequence: u64,
    #[phoxal(tag = 2)]
    pub permission: Permission,
    #[phoxal(tag = 3)]
    pub constraints: Vec<Constraint>,
    #[phoxal(tag = 4)]
    pub valid_from_nanos: u64,
    #[phoxal(tag = 5)]
    pub expires_at_nanos: u64,
    /// Oldest source capture supporting this product, in the execution timeline.
    /// Republishing or deriving state must preserve this time.
    #[phoxal(tag = 6)]
    pub oldest_capture_time_nanos: Option<u64>,
}

#[phoxal::messages(package = "phoxal.motion.v1")]
mod v1 {
    use super::{ActuatorSetpoint, Empty, Latest, MotionConstraints, OdometryState, RequestReply};

    /// A motion command in the body frame.
    pub struct MotionIntent {
        /// Forward velocity along the body x axis.
        #[phoxal(tag = 1)]
        pub linear_x_mps: f64,
        /// Counter-clockwise yaw rate around the body z axis.
        #[phoxal(tag = 2)]
        pub angular_z_radps: f64,
    }

    /// The motion product's control mode.
    pub enum ControlMode {
        Unspecified = 0,
        Disarmed = 1,
        Manual = 2,
        Autonomous = 3,
    }

    /// The motion product's status snapshot.
    pub struct MotionStatus {
        #[phoxal(tag = 1)]
        pub mode: ControlMode,
        #[phoxal(tag = 2)]
        pub emergency_latched: bool,
        #[phoxal(tag = 3)]
        pub selected_owner_id: Option<String>,
        #[phoxal(tag = 4)]
        pub protective_state_clear: bool,
        #[phoxal(tag = 5)]
        pub stopped: bool,
    }

    /// The release-emergency request payload.
    pub struct ReleaseEmergencyRequest {
        #[phoxal(tag = 1)]
        pub reset_token: String,
    }

    /// The arm request payload.
    pub struct ArmRequest {
        #[phoxal(tag = 1)]
        pub mode: ControlMode,
    }

    /// Why an emergency request was refused.
    pub enum EmergencyRefusalReason {
        Unspecified = 0,
        InvalidRequest = 1,
        ProtectiveState = 2,
        NotLatched = 3,
    }

    /// The refused variant of an emergency response.
    pub struct EmergencyRefused {
        #[phoxal(tag = 1)]
        pub reason: EmergencyRefusalReason,
    }

    /// One arm/disarm/emergency response.
    pub enum ApplyEmergencyResponse {
        #[phoxal(tag = 1)]
        Accepted,
        #[phoxal(tag = 2)]
        Refused(EmergencyRefused),
    }

    /// The motion service's endpoint contract.
    #[phoxal::endpoints]
    pub struct MotionApi {
        #[phoxal::input(lease_ms = 100, max_bytes = 4096)]
        manual: Latest<MotionIntent>,

        #[phoxal::input(lease_ms = 100, max_bytes = 4096)]
        autonomous: Latest<MotionIntent>,

        #[phoxal::input(max_age_ms = 100, max_bytes = 4096)]
        safety: Latest<MotionConstraints>,

        #[phoxal::input(max_age_ms = 100, max_bytes = 512)]
        measurements: Latest<OdometryState>,

        #[phoxal::output(projection = state, lease_ms = 100, max_bytes = 1024)]
        actuators: Latest<ActuatorSetpoint>,

        #[phoxal::output(projection = state, bootstrap, max_bytes = 512)]
        status: Latest<MotionStatus>,

        #[phoxal::operation(max_items = 32, max_bytes = 16_384)]
        arm: RequestReply<ArmRequest, ApplyEmergencyResponse>,

        #[phoxal::operation(max_items = 32, max_bytes = 16_384)]
        disarm: RequestReply<Empty, ApplyEmergencyResponse>,

        #[phoxal::operation(max_items = 32, max_bytes = 16_384)]
        engage_emergency: RequestReply<Empty, ApplyEmergencyResponse>,

        #[phoxal::operation(max_items = 32, max_bytes = 16_384)]
        release_emergency: RequestReply<ReleaseEmergencyRequest, ApplyEmergencyResponse>,
    }
}

pub use v1::*;
