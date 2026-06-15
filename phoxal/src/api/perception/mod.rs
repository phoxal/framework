pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Detections {
    #[serde(rename = "1")]
    V1(v1::Detections),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum PerceptionState {
    #[serde(rename = "1")]
    V1(v1::PerceptionState),
}
