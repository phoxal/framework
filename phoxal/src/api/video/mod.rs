pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum OpenRequest {
    #[serde(rename = "1")]
    V1(v1::OpenRequest),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum OpenResponse {
    #[serde(rename = "1")]
    V1(v1::OpenResponse),
}

impl crate::bus::zenoh::BusyResponse for OpenResponse {
    fn busy() -> Self {
        Self::V1(<v1::OpenResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum StreamEvent {
    #[serde(rename = "1")]
    V1(v1::StreamEvent),
}
