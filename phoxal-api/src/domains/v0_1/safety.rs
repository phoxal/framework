//! v0.1 safety payloads.
#![allow(legacy_derive_helpers)]

/// Why safety is stopping or limiting body motion.
#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintReason {
    WorldUnavailable,
    MapUnavailable,
    DrivableSpaceUnavailable,
    LocalizationUnavailable,
    LocalizationUncertain,
    ObstacleProximity,
    RangeSensorFault,
    DriveFault,
    BatteryLow,
    BatteryCritical,
    SpeedZone,
    OperatorPolicy,
}

/// Typed origin of one constraint, suitable for operator diagnosis.
#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstraintSourceKind {
    WorldModel,
    Map,
    Localization,
    Range,
    Drive,
    Battery,
    Operator,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConstraintSource {
    pub kind: ConstraintSourceKind,
    pub participant_id: String,
    pub component_id: Option<String>,
    pub capability_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Constraint {
    pub reason: ConstraintReason,
    pub source: ConstraintSource,
    pub stop: bool,
    pub max_linear_speed_mps: Option<f32>,
    pub max_angular_speed_radps: Option<f32>,
    pub observed_value: Option<f32>,
    /// The instant this constraint starts applying, on the
    /// publisher's timeline. A consumer on another timeline gets a
    /// checked error, never a silently wrong comparison.
    pub valid_from: ::phoxal_bus::RobotInstant,
    /// The instant this constraint stops applying.
    pub expires_at: ::phoxal_bus::RobotInstant,
}

/// The sole safety-to-motion control product. Motion accepts it only
/// on the same timeline and before `expires_at`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MotionConstraints {
    pub sequence: u64,
    pub stop: bool,
    pub max_linear_speed_mps: Option<f32>,
    pub max_angular_speed_radps: Option<f32>,
    pub constraints: Vec<Constraint>,
    pub expires_at: ::phoxal_bus::RobotInstant,
}

/// Operator-facing state mirrors the exact product consumed by motion.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    pub clear: bool,
    pub motion: MotionConstraints,
}
