pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum MapRevision {
    #[serde(rename = "1")]
    V1(v1::MapRevision),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Summary {
    #[serde(rename = "1")]
    V1(v1::Summary),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum LocalCost {
    #[serde(rename = "1")]
    V1(v1::LocalCost),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Traversability {
    #[serde(rename = "1")]
    V1(v1::Traversability),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum TraversabilitySummary {
    #[serde(rename = "1")]
    V1(v1::TraversabilitySummary),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum SubmapRequest {
    #[serde(rename = "1")]
    V1(v1::SubmapRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum SubmapResponse {
    #[serde(rename = "1")]
    V1(v1::SubmapResponse),
}

impl crate::bus::zenoh::BusyResponse for SubmapResponse {
    fn busy() -> Self {
        Self::V1(<v1::SubmapResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum EsdfTileRequest {
    #[serde(rename = "1")]
    V1(v1::EsdfTileRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum EsdfTileResponse {
    #[serde(rename = "1")]
    V1(v1::EsdfTileResponse),
}

impl crate::bus::zenoh::BusyResponse for EsdfTileResponse {
    fn busy() -> Self {
        Self::V1(<v1::EsdfTileResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum TraversabilityTileRequest {
    #[serde(rename = "1")]
    V1(v1::TraversabilityTileRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum TraversabilityTileResponse {
    #[serde(rename = "1")]
    V1(v1::TraversabilityTileResponse),
}

impl crate::bus::zenoh::BusyResponse for TraversabilityTileResponse {
    fn busy() -> Self {
        Self::V1(<v1::TraversabilityTileResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum LocalGridRequest {
    #[serde(rename = "1")]
    V1(v1::LocalGridRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum LocalGridResponse {
    #[serde(rename = "1")]
    V1(v1::LocalGridResponse),
}

impl crate::bus::zenoh::BusyResponse for LocalGridResponse {
    fn busy() -> Self {
        Self::V1(<v1::LocalGridResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum GlobalGridRequest {
    #[serde(rename = "1")]
    V1(v1::GlobalGridRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum GlobalGridResponse {
    #[serde(rename = "1")]
    V1(v1::GlobalGridResponse),
}

impl crate::bus::zenoh::BusyResponse for GlobalGridResponse {
    fn busy() -> Self {
        Self::V1(<v1::GlobalGridResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum SnapshotRequest {
    #[serde(rename = "1")]
    V1(v1::SnapshotRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum SnapshotResponse {
    #[serde(rename = "1")]
    V1(v1::SnapshotResponse),
}

impl crate::bus::zenoh::BusyResponse for SnapshotResponse {
    fn busy() -> Self {
        Self::V1(<v1::SnapshotResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}
