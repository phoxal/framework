pub const SCHEMA_NAME: &str = "phoxal-api-follow/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::api::localize::v1::LocalizationRevisionId;
use crate::api::map::v1::MapRevisionId;
use crate::bus::zenoh_typed::TypedSchema;
use serde::{Deserialize, Serialize};

pub const TARGET_TOPIC: &str = "runtime/follow/target";
pub const STATE_TOPIC: &str = "runtime/follow/state";
pub const DEBUG_TRACKING_ERROR_TOPIC: &str = "runtime/follow/debug/tracking_error";
pub const DEBUG_CANDIDATES_TOPIC: &str = "runtime/follow/debug/candidates";
pub const DEBUG_COSTS_TOPIC: &str = "runtime/follow/debug/costs";
pub const DEBUG_REVISION_INPUTS_TOPIC: &str = "runtime/follow/debug/revision_inputs";
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub map_revision: MapRevisionId,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub frame_id: String,
    pub linear_x_mps: f64,
    pub angular_z_radps: f64,
}

impl TypedSchema for Target {
    const SCHEMA_NAME: &'static str = "runtime/follow/target";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub status: FollowStatus,
    pub reason: Option<FollowReason>,
}

impl TypedSchema for State {
    const SCHEMA_NAME: &'static str = "runtime/follow/state";
    const SCHEMA_VERSION: u32 = 2;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowStatus {
    Idle,
    Tracking,
    Paused,
    Refused,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowReason {
    NoLocalizationState,
    LocalizationInitializing,
    LocalizationLost,
    LocalizationRelocalizing,
    UnsupportedLocalizationMode,
    PathLocalizeRevisionMismatch,
    LocalizationRevisionUnknown,
    NoLocalizationPose,
    Arrived,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackingError {
    pub lateral_m: f64,
    pub heading_rad: f64,
}

impl TypedSchema for TrackingError {
    const SCHEMA_NAME: &'static str = "runtime/follow/debug/tracking_error";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidates {
    pub targets: Vec<Target>,
}

impl TypedSchema for Candidates {
    const SCHEMA_NAME: &'static str = "runtime/follow/debug/candidates";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Costs {
    pub costs: Vec<Cost>,
}

impl TypedSchema for Costs {
    const SCHEMA_NAME: &'static str = "runtime/follow/debug/costs";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub name: String,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionInputs {
    pub map_revision: Option<MapRevisionId>,
    pub localization_revision: Option<LocalizationRevisionId>,
}

impl TypedSchema for RevisionInputs {
    const SCHEMA_NAME: &'static str = "runtime/follow/debug/revision_inputs";
    const SCHEMA_VERSION: u32 = 1;
}

crate::bus::pubsub_leaf!(target, TARGET_TOPIC, Target);
crate::bus::pubsub_leaf!(state, STATE_TOPIC, State);

pub mod debug {
    use super::*;

    crate::bus::pubsub_leaf!(tracking_error, DEBUG_TRACKING_ERROR_TOPIC, TrackingError);
    crate::bus::pubsub_leaf!(candidates, DEBUG_CANDIDATES_TOPIC, Candidates);
    crate::bus::pubsub_leaf!(costs, DEBUG_COSTS_TOPIC, Costs);
    crate::bus::pubsub_leaf!(revision_inputs, DEBUG_REVISION_INPUTS_TOPIC, RevisionInputs);
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh_typed::TypedSchema;

    use super::{
        Candidates, Costs, RevisionInputs, SCHEMA_NAME, SCHEMA_VERSION, State, Target,
        TrackingError,
    };

    #[test]
    fn schema_contracts_do_not_drift() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-follow/v1");
        assert_eq!(SCHEMA_VERSION, 1);
        assert_eq!(Target::SCHEMA_NAME, "runtime/follow/target");
        assert_eq!(Target::SCHEMA_VERSION, 1);
        assert_eq!(State::SCHEMA_NAME, "runtime/follow/state");
        assert_eq!(State::SCHEMA_VERSION, 2);
        assert_eq!(
            TrackingError::SCHEMA_NAME,
            "runtime/follow/debug/tracking_error"
        );
        assert_eq!(TrackingError::SCHEMA_VERSION, 1);
        assert_eq!(Candidates::SCHEMA_NAME, "runtime/follow/debug/candidates");
        assert_eq!(Candidates::SCHEMA_VERSION, 1);
        assert_eq!(Costs::SCHEMA_NAME, "runtime/follow/debug/costs");
        assert_eq!(Costs::SCHEMA_VERSION, 1);
        assert_eq!(
            RevisionInputs::SCHEMA_NAME,
            "runtime/follow/debug/revision_inputs"
        );
        assert_eq!(RevisionInputs::SCHEMA_VERSION, 1);
    }
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-follow/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
