pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Target {
    #[serde(rename = "1")]
    V1(v1::Target),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum State {
    #[serde(rename = "1")]
    V1(v1::State),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum TrackingError {
    #[serde(rename = "1")]
    V1(v1::TrackingError),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Candidates {
    #[serde(rename = "1")]
    V1(v1::Candidates),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Costs {
    #[serde(rename = "1")]
    V1(v1::Costs),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum RevisionInputs {
    #[serde(rename = "1")]
    V1(v1::RevisionInputs),
}
