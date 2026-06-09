pub const SCHEMA_NAME: &str = "phoxal-api-safety/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::api::localize::v1::LocalizationRevisionId;
use crate::api::map::v1::MapRevisionId;
use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SafetyAuthorization {
    pub decision: SafetyDecision,
    pub source_revision: SafetySourceRevision,
    pub approved_motion: MotionConstraint,
    pub reasons: Vec<SafetyReason>,
    pub expires_at_ns: Option<u64>,
}

impl TypedSchema for SafetyAuthorization {
    const SCHEMA_NAME: &'static str = "runtime/safety/authorization";
    const SCHEMA_VERSION: u32 = 1;
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

impl TypedSchema for State {
    const SCHEMA_NAME: &'static str = "runtime/safety/state";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmergencyStopRequest {
    pub engaged: bool,
}

impl TypedSchema for EmergencyStopRequest {
    const SCHEMA_NAME: &'static str = "runtime/safety/emergency_stop_request";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub source_revision: SafetySourceRevision,
    pub points_m: Vec<[f64; 3]>,
    pub regions: Vec<EvidenceRegion>,
}

impl TypedSchema for Evidence {
    const SCHEMA_NAME: &'static str = "runtime/safety/debug/evidence";
    const SCHEMA_VERSION: u32 = 1;
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

impl TypedSchema for StopSet {
    const SCHEMA_NAME: &'static str = "runtime/safety/debug/stop_set";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyBudget {
    pub sources: Vec<SourceLatency>,
}

impl TypedSchema for LatencyBudget {
    const SCHEMA_NAME: &'static str = "runtime/safety/debug/latency_budget";
    const SCHEMA_VERSION: u32 = 1;
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

impl TypedSchema for SourceHealth {
    const SCHEMA_NAME: &'static str = "runtime/safety/debug/source_health";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStatus {
    pub source_id: String,
    pub healthy: bool,
    pub reason: Option<String>,
}

crate::bus::topic_leaf! {
    pubsub authorization {
        path: "runtime/safety/authorization",
        payload: SafetyAuthorization
    }
}

crate::bus::topic_leaf! {
    pubsub state {
        path: "runtime/safety/state",
        payload: State
    }
}

crate::bus::topic_leaf! {
    pubsub emergency_stop_request {
        path: "runtime/safety/emergency_stop_request",
        payload: EmergencyStopRequest
    }
}

pub mod debug {
    use super::*;

    crate::bus::topic_leaf! {
        pubsub evidence {
            path: "runtime/safety/debug/evidence",
            payload: Evidence
        }
    }

    crate::bus::topic_leaf! {
        pubsub stop_set {
            path: "runtime/safety/debug/stop_set",
            payload: StopSet
        }
    }

    crate::bus::topic_leaf! {
        pubsub latency_budget {
            path: "runtime/safety/debug/latency_budget",
            payload: LatencyBudget
        }
    }

    crate::bus::topic_leaf! {
        pubsub source_health {
            path: "runtime/safety/debug/source_health",
            payload: SourceHealth
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::{
        EmergencyStopRequest, Evidence, LatencyBudget, SCHEMA_NAME, SCHEMA_VERSION,
        SafetyAuthorization, SourceHealth, State, StopSet,
    };

    #[test]
    fn schema_contracts_do_not_drift() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-safety/v1");
        assert_eq!(SCHEMA_VERSION, 1);
        assert_eq!(
            SafetyAuthorization::SCHEMA_NAME,
            "runtime/safety/authorization"
        );
        assert_eq!(SafetyAuthorization::SCHEMA_VERSION, 1);
        assert_eq!(State::SCHEMA_NAME, "runtime/safety/state");
        assert_eq!(State::SCHEMA_VERSION, 1);
        assert_eq!(
            EmergencyStopRequest::SCHEMA_NAME,
            "runtime/safety/emergency_stop_request"
        );
        assert_eq!(EmergencyStopRequest::SCHEMA_VERSION, 1);
        assert_eq!(Evidence::SCHEMA_NAME, "runtime/safety/debug/evidence");
        assert_eq!(Evidence::SCHEMA_VERSION, 1);
        assert_eq!(StopSet::SCHEMA_NAME, "runtime/safety/debug/stop_set");
        assert_eq!(StopSet::SCHEMA_VERSION, 1);
        assert_eq!(
            LatencyBudget::SCHEMA_NAME,
            "runtime/safety/debug/latency_budget"
        );
        assert_eq!(LatencyBudget::SCHEMA_VERSION, 1);
        assert_eq!(
            SourceHealth::SCHEMA_NAME,
            "runtime/safety/debug/source_health"
        );
        assert_eq!(SourceHealth::SCHEMA_VERSION, 1);
    }

    #[test]
    fn topic_paths_are_stable() {
        assert_eq!(super::authorization::path(), "runtime/safety/authorization");
        assert_eq!(super::state::path(), "runtime/safety/state");
        assert_eq!(
            super::emergency_stop_request::path(),
            "runtime/safety/emergency_stop_request"
        );
        assert_eq!(
            super::debug::evidence::path(),
            "runtime/safety/debug/evidence"
        );
        assert_eq!(
            super::debug::stop_set::path(),
            "runtime/safety/debug/stop_set"
        );
        assert_eq!(
            super::debug::latency_budget::path(),
            "runtime/safety/debug/latency_budget"
        );
        assert_eq!(
            super::debug::source_health::path(),
            "runtime/safety/debug/source_health"
        );
    }
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
