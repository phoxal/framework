pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Heartbeat {
    #[serde(rename = "1")]
    V1(v1::Heartbeat),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Summary {
    #[serde(rename = "1")]
    V1(v1::Summary),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum DebugReadiness {
    #[serde(rename = "1")]
    V1(v1::DebugReadiness),
}
