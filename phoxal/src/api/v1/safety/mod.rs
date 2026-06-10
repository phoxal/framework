pub const SCHEMA_NAME: &str = "phoxal-api-safety/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::api::v1::localize::LocalizationRevisionId;
use crate::api::v1::map::MapRevisionId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SafetyAuthorization {
    pub decision: SafetyDecision,
    pub source_revision: SafetySourceRevision,
    pub approved_motion: MotionConstraint,
    pub reasons: Vec<SafetyReason>,
    pub expires_at_ns: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SafetyDecision {
    Allow,
    Slow,
    Stop,
    /// Unconditional hard stop that wins over *every* motion source, including manual
    /// teleop (see `phoxal-runtime-motion`). It is distinct from `Stop`, which is a
    /// protective stop that still permits an escape envelope (reverse + in-place
    /// rotation). `phoxal-runtime-safety` produces it from a hardware emergency-stop
    /// capability or an operator emergency-stop request.
    EmergencyStop,
    UnknownConservative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetySourceRevision {
    pub localization: Option<LocalizationRevisionId>,
    pub map: Option<MapRevisionId>,
    pub raw_sources: Vec<RawSourceRevision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawSourceRevision {
    pub source_id: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MotionConstraint {
    pub linear_x_mps: Constraint,
    pub angular_z_radps: Constraint,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Constraint {
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyReason {
    pub code: SafetyReasonCode,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyReasonCode {
    Clear,
    Obstacle,
    MissingSupport,
    StaleSource,
    LatencyExceeded,
    EmergencyStop,
    LocalizationMode,
    UnknownSpace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub decision: SafetyDecision,
    pub active_reasons: Vec<SafetyReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmergencyStopRequest {
    pub engaged: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub source_revision: SafetySourceRevision,
    pub points_m: Vec<[f64; 3]>,
    pub regions: Vec<EvidenceRegion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRegion {
    pub frame_id: String,
    pub min_xyz_m: [f64; 3],
    pub max_xyz_m: [f64; 3],
    pub reason: SafetyReasonCode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StopSet {
    pub regions: Vec<EvidenceRegion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyBudget {
    pub sources: Vec<SourceLatency>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceLatency {
    pub source_id: String,
    pub measured_latency_ns: Option<u64>,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceHealth {
    pub sources: Vec<SourceStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStatus {
    pub source_id: String,
    pub healthy: bool,
    pub reason: Option<String>,
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-safety/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
