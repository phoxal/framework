//! The safety service's payload vocabulary and endpoint contract.
//!
//! World and Motion observations are private input expectations.
//! Safety owns its published constraint and status records under
//! `phoxal.safety.v1`. Robot composition converts independently owned
//! endpoint payloads where their typed contracts differ.

use phoxal::contracts::component::range::RangeSample;
use phoxal::contracts::{Latest, Queue};

/// The tracked world pose belief.
#[phoxal::message]
pub struct WorldBelief {
    #[phoxal(tag = 1)]
    pub frame_id: String,
    #[phoxal(tag = 2)]
    pub x_m: f64,
    #[phoxal(tag = 3)]
    pub y_m: f64,
    #[phoxal(tag = 4)]
    pub yaw_rad: f64,
    #[phoxal(tag = 5)]
    pub confidence: f32,
    #[phoxal(tag = 6)]
    pub revision: u64,
    #[phoxal(tag = 7)]
    pub available: bool,
    /// Oldest source capture supporting this product, in the execution timeline.
    /// Republishing or deriving state must preserve this time.
    #[phoxal(tag = 8)]
    pub oldest_capture_time_nanos: Option<u64>,
}

/// The world product's revision snapshot.
#[phoxal::message]
pub struct WorldRevision {
    #[phoxal(tag = 1)]
    pub revision: u64,
    #[phoxal(tag = 2)]
    pub available: bool,
    /// Oldest source capture supporting this product, in the execution timeline.
    /// Republishing or deriving state must preserve this time.
    #[phoxal(tag = 3)]
    pub oldest_capture_time_nanos: Option<u64>,
}

/// The motion product's status snapshot.
#[phoxal::message]
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

/// The motion product's control mode.
#[phoxal::message]
pub enum ControlMode {
    Unspecified = 0,
    Disarmed = 1,
    Manual = 2,
    Autonomous = 3,
}

#[phoxal::messages(package = "phoxal.safety.v1")]
mod v1 {
    use super::{Latest, MotionStatus, Queue, RangeSample, WorldBelief, WorldRevision};

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
    pub enum Permission {
        Unspecified = 0,
        Clear = 1,
        Limited = 2,
        Stopped = 3,
    }

    /// The complete motion constraint product.
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

    /// The safety product's status snapshot.
    pub struct SafetyStatus {
        #[phoxal(tag = 1)]
        pub protective_state_clear: bool,
        #[phoxal(tag = 2)]
        pub sequence: u64,
        #[phoxal(tag = 3)]
        pub reasons: Vec<ConstraintReason>,
    }

    /// The safety service's endpoint contract.
    #[phoxal::endpoints]
    pub struct SafetyApi {
        #[phoxal::input(max_age_ms = 100, max_bytes = 16_384)]
        world: Latest<WorldBelief>,

        #[phoxal::input(max_age_ms = 100, max_bytes = 512)]
        world_revision: Latest<WorldRevision>,

        #[phoxal::input(max_age_ms = 100, max_bytes = 512)]
        motion: Latest<MotionStatus>,

        #[phoxal::input(max_items = 256, max_bytes = 131_072)]
        ranges: Queue<RangeSample>,

        #[phoxal::output(projection = state, bootstrap, on_change, max_bytes = 4096)]
        constraints: Latest<MotionConstraints>,

        #[phoxal::output(projection = state, bootstrap, on_change, max_bytes = 1024)]
        status: Latest<SafetyStatus>,
    }
}

pub use v1::*;
