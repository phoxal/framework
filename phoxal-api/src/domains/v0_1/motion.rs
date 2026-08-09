//! v0.1 motion payloads.
#![allow(legacy_derive_helpers)]

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Target {
    pub linear_x_mps: f32,
    pub angular_z_radps: f32,
    pub curvature_limit_radpm: Option<f32>,
}

#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Manual,
    Navigation,
    EmergencyStop,
}

#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZeroReason {
    NoCandidate,
    NavigationCandidateStale,
    ManualCandidateNotFinite,
    NavigationCandidateNotFinite,
    EmergencyStopEngaged,
    SafetyConstraintsUnavailable,
    SafetyProtectiveStop,
}

#[derive(Copy, Eq, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyRuntime {
    Absent,
    Present,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ManualCommand {
    pub linear_x_mps: f64,
    pub angular_z_radps: f64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    /// How long ago motion observed the live manual command, on
    /// its own host clock. `None` when no manual command is live.
    pub manual_observed_age_ns: Option<u64>,
    pub autonomous_candidate_age_ns: Option<u64>,
    pub safety_constraints_age_ns: Option<u64>,
    pub selected_source: Option<Source>,
    pub final_target: Target,
    pub zero_reason: Option<ZeroReason>,
    pub safety_runtime: SafetyRuntime,
    pub component_estop_blocked: bool,
    pub active_safety_constraints: Vec<super::safety::Constraint>,
}
