pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum SafetyAuthorization {
    #[serde(rename = "1")]
    V1(v1::SafetyAuthorization),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum State {
    #[serde(rename = "1")]
    V1(v1::State),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum EmergencyStopRequest {
    #[serde(rename = "1")]
    V1(v1::EmergencyStopRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Evidence {
    #[serde(rename = "1")]
    V1(v1::Evidence),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum StopSet {
    #[serde(rename = "1")]
    V1(v1::StopSet),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum LatencyBudget {
    #[serde(rename = "1")]
    V1(v1::LatencyBudget),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum SourceHealth {
    #[serde(rename = "1")]
    V1(v1::SourceHealth),
}
