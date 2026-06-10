pub const SCHEMA_NAME: &str = "phoxal-api-asset/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::bus::zenoh::BusyResponse;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetRequest {
    pub path: String,
}

impl GetRequest {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GetResponse {
    Ok { bytes: Vec<u8> },
    NotFound,
    InvalidPath(InvalidPathReason),
    Unavailable(UnavailableReason),
    Busy,
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvalidPathReason {
    Empty,
    ParentTraversal,
    BackslashSeparator,
    EmptyComponent,
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    Io,
}

impl BusyResponse for GetResponse {
    fn busy() -> Self {
        Self::Busy
    }
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-asset/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
