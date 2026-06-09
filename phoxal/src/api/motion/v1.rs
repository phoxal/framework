pub const SCHEMA_NAME: &str = "phoxal-api-motion/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::{TypedPublisherBuilder, TypedSchema, TypedSubscriberBuilder};
use serde::{Deserialize, Serialize};

pub const MANUAL_COMMAND_TOPIC: &str = "runtime/motion/manual";
pub const DRIVE_TARGET_TOPIC: &str = crate::api::drive::v1::TARGET_TOPIC;
pub const DEBUG_ARBITRATION_TOPIC: &str = "runtime/motion/debug/arbitration";
pub const DEBUG_SOURCE_FRESHNESS_TOPIC: &str = "runtime/motion/debug/source_freshness";
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

pub mod manual {
    use super::*;

    pub const TOPIC: &str = MANUAL_COMMAND_TOPIC;

    pub fn topic(bus: &crate::bus::Bus) -> String {
        bus.topic(TOPIC)
    }

    pub fn publisher(
        bus: &crate::bus::Bus,
    ) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<ManualCommand>>> {
        crate::bus::pubsub::publisher_builder(bus, TOPIC)
    }

    pub fn subscriber_builder(
        bus: &crate::bus::Bus,
    ) -> TypedSubscriberBuilder<'_, 'static, Stamped<ManualCommand>> {
        crate::bus::pubsub::subscriber_builder(bus, TOPIC)
    }
}

pub mod drive {
    pub const TARGET_TOPIC: &str = crate::api::motion::v1::DRIVE_TARGET_TOPIC;
}

pub mod debug {
    use super::*;

    crate::bus::pubsub_leaf!(arbitration, DEBUG_ARBITRATION_TOPIC, Arbitration);
    crate::bus::pubsub_leaf!(
        source_freshness,
        DEBUG_SOURCE_FRESHNESS_TOPIC,
        SourceFreshness
    );
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
