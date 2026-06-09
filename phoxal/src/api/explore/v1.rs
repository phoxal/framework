pub const SCHEMA_NAME: &str = "phoxal-api-explore/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::api::localize::v1::LocalizationRevisionId;
use crate::api::map::v1::MapRevisionId;
use crate::api::mission::v1::{GoalPose, GoalTolerance};
use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frontiers {
    pub map_revision: MapRevisionId,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub frontiers: Vec<Frontier>,
}

impl TypedSchema for Frontiers {
    const SCHEMA_NAME: &'static str = "runtime/explore/frontiers";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frontier {
    pub id: String,
    pub frame_id: String,
    pub points_xy_m: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalCandidates {
    pub map_revision: MapRevisionId,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub candidates: Vec<GoalCandidate>,
}

impl TypedSchema for GoalCandidates {
    const SCHEMA_NAME: &'static str = "runtime/explore/goal_candidates";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalCandidate {
    pub id: String,
    pub goal: GoalPose,
    pub tolerance: GoalTolerance,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub status: ExploreStatus,
    pub reason: Option<String>,
}

impl TypedSchema for State {
    const SCHEMA_NAME: &'static str = "runtime/explore/state";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExploreStatus {
    Idle,
    Evaluating,
    Ready,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scoring {
    pub scores: Vec<CandidateScore>,
}

impl TypedSchema for Scoring {
    const SCHEMA_NAME: &'static str = "runtime/explore/debug/scoring";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateScore {
    pub candidate_id: String,
    pub score: f64,
    pub factors: Vec<ScoreFactor>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreFactor {
    pub name: String,
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedCandidates {
    pub rejected: Vec<RejectedCandidate>,
}

impl TypedSchema for RejectedCandidates {
    const SCHEMA_NAME: &'static str = "runtime/explore/debug/rejected_candidates";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedCandidate {
    pub candidate_id: String,
    pub reason: String,
}

crate::bus::topic_leaf! {
    pubsub frontiers {
        path: "runtime/explore/frontiers",
        payload: Frontiers
    }
}

crate::bus::topic_leaf! {
    pubsub goal_candidates {
        path: "runtime/explore/goal_candidates",
        payload: GoalCandidates
    }
}

crate::bus::topic_leaf! {
    pubsub state {
        path: "runtime/explore/state",
        payload: State
    }
}

pub mod debug {
    use super::*;

    crate::bus::topic_leaf! {
        pubsub scoring {
            path: "runtime/explore/debug/scoring",
            payload: Scoring
        }
    }

    crate::bus::topic_leaf! {
        pubsub rejected_candidates {
            path: "runtime/explore/debug/rejected_candidates",
            payload: RejectedCandidates
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::{
        Frontiers, GoalCandidates, RejectedCandidates, SCHEMA_NAME, SCHEMA_VERSION, Scoring, State,
    };

    #[test]
    fn schema_contracts_do_not_drift() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-explore/v1");
        assert_eq!(SCHEMA_VERSION, 1);
        assert_eq!(Frontiers::SCHEMA_NAME, "runtime/explore/frontiers");
        assert_eq!(Frontiers::SCHEMA_VERSION, 1);
        assert_eq!(
            GoalCandidates::SCHEMA_NAME,
            "runtime/explore/goal_candidates"
        );
        assert_eq!(GoalCandidates::SCHEMA_VERSION, 1);
        assert_eq!(State::SCHEMA_NAME, "runtime/explore/state");
        assert_eq!(State::SCHEMA_VERSION, 1);
        assert_eq!(Scoring::SCHEMA_NAME, "runtime/explore/debug/scoring");
        assert_eq!(Scoring::SCHEMA_VERSION, 1);
        assert_eq!(
            RejectedCandidates::SCHEMA_NAME,
            "runtime/explore/debug/rejected_candidates"
        );
        assert_eq!(RejectedCandidates::SCHEMA_VERSION, 1);
    }

    #[test]
    fn topic_paths_are_stable() {
        assert_eq!(super::frontiers::path(), "runtime/explore/frontiers");
        assert_eq!(
            super::goal_candidates::path(),
            "runtime/explore/goal_candidates"
        );
        assert_eq!(super::state::path(), "runtime/explore/state");
        assert_eq!(
            super::debug::scoring::path(),
            "runtime/explore/debug/scoring"
        );
        assert_eq!(
            super::debug::rejected_candidates::path(),
            "runtime/explore/debug/rejected_candidates"
        );
    }
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-explore/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
