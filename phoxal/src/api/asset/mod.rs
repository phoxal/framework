pub mod v1;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum GetRequest {
    #[serde(rename = "1")]
    V1(v1::GetRequest),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum GetResponse {
    #[serde(rename = "1")]
    V1(v1::GetResponse),
}

impl crate::bus::zenoh::BusyResponse for GetResponse {
    fn busy() -> Self {
        Self::V1(<v1::GetResponse as crate::bus::zenoh::BusyResponse>::busy())
    }
}
