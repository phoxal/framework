pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum LocalizationState {
    #[serde(rename = "1")]
    V1(v1::LocalizationState),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum PoseEstimate {
    #[serde(rename = "1")]
    V1(v1::PoseEstimate),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum LocalizationRevision {
    #[serde(rename = "1")]
    V1(v1::LocalizationRevision),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Keyframe {
    #[serde(rename = "1")]
    V1(v1::Keyframe),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum PoseGraphCorrection {
    #[serde(rename = "1")]
    V1(v1::PoseGraphCorrection),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum PoseGraphRequest {
    #[serde(rename = "1")]
    V1(v1::PoseGraphRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum PoseGraphResponse {
    #[serde(rename = "1")]
    V1(v1::PoseGraphResponse),
}

impl crate::bus::zenoh::BusyResponse for PoseGraphResponse {
    fn busy() -> Self {
        Self::V1(<v1::PoseGraphResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum KeyframeRequest {
    #[serde(rename = "1")]
    V1(v1::KeyframeRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum KeyframeResponse {
    #[serde(rename = "1")]
    V1(v1::KeyframeResponse),
}

impl crate::bus::zenoh::BusyResponse for KeyframeResponse {
    fn busy() -> Self {
        Self::V1(<v1::KeyframeResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum CorrectionsRequest {
    #[serde(rename = "1")]
    V1(v1::CorrectionsRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum CorrectionsResponse {
    #[serde(rename = "1")]
    V1(v1::CorrectionsResponse),
}

impl crate::bus::zenoh::BusyResponse for CorrectionsResponse {
    fn busy() -> Self {
        Self::V1(<v1::CorrectionsResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}
