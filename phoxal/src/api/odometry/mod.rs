pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum OdometryEstimate {
    #[serde(rename = "1")]
    V1(v1::OdometryEstimate),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Status {
    #[serde(rename = "1")]
    V1(v1::Status),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum SourceHealth {
    #[serde(rename = "1")]
    V1(v1::SourceHealth),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Residuals {
    #[serde(rename = "1")]
    V1(v1::Residuals),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Integration {
    #[serde(rename = "1")]
    V1(v1::Integration),
}
