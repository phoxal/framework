pub const SCHEMA_NAME: &str = "phoxal-api-asset/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::bus::zenoh::{BusyResponse, TypedSchema};
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

impl TypedSchema for GetRequest {
    const SCHEMA_NAME: &'static str = "runtime/asset/get/request";
    const SCHEMA_VERSION: u32 = 1;
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

impl TypedSchema for GetResponse {
    const SCHEMA_NAME: &'static str = "runtime/asset/get/response";
    const SCHEMA_VERSION: u32 = 1;
}

impl BusyResponse for GetResponse {
    fn busy() -> Self {
        Self::Busy
    }
}

pub mod get {
    use super::{GetRequest, GetResponse};

    crate::bus::topic_leaf! {
        query {
            path: "runtime/asset/get",
            request: GetRequest,
            response: GetResponse
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GetRequest, GetResponse};
    use crate::bus::zenoh::TypedSchema;

    #[test]
    fn get_request_schema_contract_is_stable() {
        assert_eq!(GetRequest::SCHEMA_NAME, "runtime/asset/get/request");
        assert_eq!(GetRequest::SCHEMA_VERSION, 1);
    }

    #[test]
    fn get_response_schema_contract_is_stable() {
        assert_eq!(GetResponse::SCHEMA_NAME, "runtime/asset/get/response");
        assert_eq!(GetResponse::SCHEMA_VERSION, 1);
    }

    #[test]
    fn get_path_is_stable() {
        assert_eq!(super::get::path(), "runtime/asset/get");
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
