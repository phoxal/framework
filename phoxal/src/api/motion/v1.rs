pub const SCHEMA_NAME: &str = "phoxal-api-motion/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub active_source: Option<MotionSource>,
    pub selected: Option<crate::api::drive::v1::Target>,
    pub reason: Option<MotionReason>,
}

impl TypedSchema for State {
    const SCHEMA_NAME: &'static str = "runtime/motion/state";
    const SCHEMA_VERSION: u32 = 2;
}

/// Operator manual velocity command (host joypad/teleop). Consumed by the motion runtime as a
/// top-priority source, still clamped to the safety-approved envelope and overridden by safety Stop.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ManualCommand {
    pub linear_x_mps: f64,
    pub angular_z_radps: f64,
}

impl TypedSchema for ManualCommand {
    const SCHEMA_NAME: &'static str = "runtime/motion/manual";
    const SCHEMA_VERSION: u32 = 1;
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

impl TypedSchema for Arbitration {
    const SCHEMA_NAME: &'static str = "runtime/motion/debug/arbitration";
    const SCHEMA_VERSION: u32 = 1;
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

impl TypedSchema for SourceFreshness {
    const SCHEMA_NAME: &'static str = "runtime/motion/debug/source_freshness";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceStatus {
    pub source: MotionSource,
    pub fresh: bool,
    pub reason: Option<String>,
}

crate::bus::topic_leaf! {
    pubsub state() {
        path: "runtime/motion/state",
        payload: State
    }
}

crate::bus::topic_leaf! {
    pubsub manual {
        path: "runtime/motion/manual",
        payload: ManualCommand
    }
}

pub mod debug {
    use super::*;

    crate::bus::topic_leaf! {
        pubsub arbitration {
            path: "runtime/motion/debug/arbitration",
            payload: Arbitration
        }
    }

    crate::bus::topic_leaf! {
        pubsub source_freshness {
            path: "runtime/motion/debug/source_freshness",
            payload: SourceFreshness
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::{
        Arbitration, ManualCommand, SCHEMA_NAME, SCHEMA_VERSION, SourceFreshness, State, state,
    };

    #[test]
    fn schema_contracts_do_not_drift() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-motion/v1");
        assert_eq!(SCHEMA_VERSION, 1);
        assert_eq!(State::SCHEMA_NAME, "runtime/motion/state");
        assert_eq!(State::SCHEMA_VERSION, 2);
        assert_eq!(state::schema_name(), "runtime/motion/state");
        assert_eq!(state::schema_version(), 2);
        assert_eq!(ManualCommand::SCHEMA_NAME, "runtime/motion/manual");
        assert_eq!(ManualCommand::SCHEMA_VERSION, 1);
        assert_eq!(Arbitration::SCHEMA_NAME, "runtime/motion/debug/arbitration");
        assert_eq!(Arbitration::SCHEMA_VERSION, 1);
        assert_eq!(
            SourceFreshness::SCHEMA_NAME,
            "runtime/motion/debug/source_freshness"
        );
        assert_eq!(SourceFreshness::SCHEMA_VERSION, 1);
    }

    #[test]
    fn state_path_is_stable() {
        assert_eq!(state::path(), "runtime/motion/state");
    }

    #[test]
    fn topic_paths_are_stable() {
        assert_eq!(super::manual::path(), "runtime/motion/manual");
        assert_eq!(
            super::debug::arbitration::path(),
            "runtime/motion/debug/arbitration"
        );
        assert_eq!(
            super::debug::source_freshness::path(),
            "runtime/motion/debug/source_freshness"
        );
    }
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
