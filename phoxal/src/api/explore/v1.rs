use crate::api::localize::v1::LocalizationRevisionId;
use crate::api::map::v1::MapRevisionId;
use crate::api::mission::v1::{GoalPose, GoalTolerance};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frontiers {
    pub map_revision: MapRevisionId,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub frontiers: Vec<Frontier>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedCandidate {
    pub candidate_id: String,
    pub reason: String,
}
