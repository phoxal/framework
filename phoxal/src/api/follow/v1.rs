pub const SCHEMA_NAME: &str = "phoxal-api-follow/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::api::localize::v1::LocalizationRevisionId;
use crate::api::map::v1::MapRevisionId;
use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

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

crate::bus::topic_leaf! {
    pubsub target {
        path: "runtime/follow/target",
        payload: Target
    }
}

crate::bus::topic_leaf! {
    pubsub state {
        path: "runtime/follow/state",
        payload: State
    }
}

pub mod debug {
    use super::*;

    crate::bus::topic_leaf! {
        pubsub tracking_error {
            path: "runtime/follow/debug/tracking_error",
            payload: TrackingError
        }
    }

    crate::bus::topic_leaf! {
        pubsub candidates {
            path: "runtime/follow/debug/candidates",
            payload: Candidates
        }
    }

    crate::bus::topic_leaf! {
        pubsub costs {
            path: "runtime/follow/debug/costs",
            payload: Costs
        }
    }

    crate::bus::topic_leaf! {
        pubsub revision_inputs {
            path: "runtime/follow/debug/revision_inputs",
            payload: RevisionInputs
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

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

    #[test]
    fn topic_paths_are_stable() {
        assert_eq!(super::target::path(), "runtime/follow/target");
        assert_eq!(super::state::path(), "runtime/follow/state");
        assert_eq!(
            super::debug::tracking_error::path(),
            "runtime/follow/debug/tracking_error"
        );
        assert_eq!(
            super::debug::candidates::path(),
            "runtime/follow/debug/candidates"
        );
        assert_eq!(super::debug::costs::path(), "runtime/follow/debug/costs");
        assert_eq!(
            super::debug::revision_inputs::path(),
            "runtime/follow/debug/revision_inputs"
        );
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
