pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Tree {
    #[serde(rename = "1")]
    V1(v1::Tree),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Static {
    #[serde(rename = "1")]
    V1(v1::Static),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum FrameTransform {
    #[serde(rename = "1")]
    V1(v1::FrameTransform),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum FrameLookupRequest {
    #[serde(rename = "1")]
    V1(v1::FrameLookupRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum FrameLookupResponse {
    #[serde(rename = "1")]
    V1(v1::FrameLookupResponse),
}

impl crate::bus::zenoh::BusyResponse for FrameLookupResponse {
    fn busy() -> Self {
        Self::V1(<v1::FrameLookupResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}
