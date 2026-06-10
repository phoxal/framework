pub const SCHEMA_NAME: &str = "phoxal-api-motion/v1";
pub const SCHEMA_VERSION: u32 = 1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub active_source: Option<MotionSource>,
    pub selected: Option<crate::api::drive::v1::Target>,
    pub reason: Option<MotionReason>,
}

/// Operator manual velocity command (host joypad/teleop). Consumed by the motion runtime as a
/// top-priority source, still clamped to the safety-approved envelope and overridden by safety Stop.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ManualCommand {
    pub linear_x_mps: f64,
    pub angular_z_radps: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionSource {
    Manual,
    Follow,
    MissionStop,
    Recovery,
    EmergencyStop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionReason {
    SafetyEmergencyStop,
    ManualEscapeUnderStop,
    SafetyConstrained(crate::api::safety::v1::SafetyDecision),
    NoFollowTarget,
    FollowTargetStale,
    SafetyAuthorizationUnavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Arbitration {
    pub candidates: Vec<ArbitrationCandidate>,
    pub selected_source: Option<MotionSource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArbitrationCandidate {
    pub source: MotionSource,
    pub target: Option<crate::api::drive::v1::Target>,
    pub accepted: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFreshness {
    pub sources: Vec<SourceStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStatus {
    pub source: MotionSource,
    pub fresh: bool,
    pub reason: Option<String>,
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-motion/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
