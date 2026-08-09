//! v0.1 drive payloads.
#![allow(legacy_derive_helpers)]

/// Why actuation authority is in its current state.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StopReason {
    /// Nothing is live: no target has been accepted, the producer
    /// has gone silent past the host deadline, or the held command
    /// exceeded its logical hold horizon. All three are the same
    /// fact to a consumer - the drive is not being commanded.
    TargetStale,
    TargetNotFinite,
    ActuatorCommandNotFinite,
    Inactive,
    EmergencyStop,
    Fault,
}

/// Whether the drive is actively commanding the actuators.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ActuatorAuthority {
    Active,
    Stopped,
}

/// A requested or limited planar velocity.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Target {
    pub linear_x_mps: f32,
    pub angular_z_radps: f32,
    pub curvature_limit_radpm: Option<f32>,
}

/// The drive participant's published control state.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    pub target: Target,
    pub limited_target: Target,
    pub actuator_authority: ActuatorAuthority,
    pub stop_reason: Option<StopReason>,
}
