pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Frontiers {
    #[serde(rename = "1")]
    V1(v1::Frontiers),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum GoalCandidates {
    #[serde(rename = "1")]
    V1(v1::GoalCandidates),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum State {
    #[serde(rename = "1")]
    V1(v1::State),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Scoring {
    #[serde(rename = "1")]
    V1(v1::Scoring),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum RejectedCandidates {
    #[serde(rename = "1")]
    V1(v1::RejectedCandidates),
}
