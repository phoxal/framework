pub const SCHEMA_NAME: &str = "phoxal-api-frame/v1";
pub const SCHEMA_VERSION: u32 = 1;

use std::fmt;

use crate::api::joint::v1::JointId;
use crate::bus::zenoh::{BusyResponse, TypedSchema};
use serde::{Deserialize, Serialize};

pub const DATA_SCHEMA: &str = "runtime/frame/data";
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FrameId(pub String);

impl FrameId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for FrameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    pub revision: u64,
    pub frames: Vec<FrameLink>,
}

impl TypedSchema for Tree {
    const SCHEMA_NAME: &'static str = "runtime/frame/tree";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameLink {
    pub frame_id: FrameId,
    pub parent_frame_id: Option<FrameId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Static {
    pub transforms: Vec<FrameTransform>,
}

impl TypedSchema for Static {
    const SCHEMA_NAME: &'static str = "runtime/frame/static";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameTransform {
    pub parent_frame_id: Option<FrameId>,
    pub child_frame_id: FrameId,
    pub translation_m: [f64; 3],
    pub rotation_xyzw: [f64; 4],
    pub source: Source,
}

impl TypedSchema for FrameTransform {
    const SCHEMA_NAME: &'static str = DATA_SCHEMA;
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Source {
    Static,
    Joint { joint_id: JointId },
    Lookup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameLookupRequest {
    pub parent_frame_id: FrameId,
    pub child_frame_id: FrameId,
    pub timestamp_ns: u64,
}

impl TypedSchema for FrameLookupRequest {
    const SCHEMA_NAME: &'static str = "runtime/frame/lookup/request";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FrameLookupResponse {
    Ok {
        parent_frame_id: FrameId,
        child_frame_id: FrameId,
        timestamp_ns: u64,
        transform: FrameTransform,
    },
    UnknownFrame {
        frame_id: FrameId,
    },
    DisconnectedTree {
        parent_frame_id: FrameId,
        child_frame_id: FrameId,
    },
    ExtrapolationTooOld {
        oldest_available_ns: u64,
    },
    ExtrapolationTooNew {
        newest_available_ns: u64,
    },
    Busy,
}

impl TypedSchema for FrameLookupResponse {
    const SCHEMA_NAME: &'static str = "runtime/frame/lookup/response";
    const SCHEMA_VERSION: u32 = 1;
}

impl BusyResponse for FrameLookupResponse {
    fn busy() -> Self {
        Self::Busy
    }
}

crate::bus::topic_leaf! {
    pubsub tree {
        path: "runtime/frame/tree",
        payload: Tree
    }
}

crate::bus::topic_leaf! {
    pubsub r#static {
        path: "runtime/frame/static",
        payload: Static
    }
}

pub mod data {
    use super::*;

    pub const SCHEMA: &str = DATA_SCHEMA;

    crate::bus::topic_leaf! {
        pubsub(frame_id: &FrameId) {
            path: "runtime/frame/{}/data",
            payload: FrameTransform
        }
    }
}

crate::bus::topic_leaf! {
    query lookup() {
        path: "runtime/frame/lookup",
        request: FrameLookupRequest,
        response: FrameLookupResponse
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::{BusyResponse, TypedSchema};

    use crate::api::frame::v1::{
        FrameLookupRequest, FrameLookupResponse, FrameTransform, Static, Tree, lookup,
    };

    #[test]
    fn frame_contract_schemas_are_stable() {
        assert_eq!(Tree::SCHEMA_NAME, "runtime/frame/tree");
        assert_eq!(Tree::SCHEMA_VERSION, 1);
        assert_eq!(Static::SCHEMA_NAME, "runtime/frame/static");
        assert_eq!(Static::SCHEMA_VERSION, 1);
        assert_eq!(FrameTransform::SCHEMA_NAME, "runtime/frame/data");
        assert_eq!(FrameTransform::SCHEMA_VERSION, 1);
        assert_eq!(
            FrameLookupRequest::SCHEMA_NAME,
            "runtime/frame/lookup/request"
        );
        assert_eq!(FrameLookupRequest::SCHEMA_VERSION, 1);
        assert_eq!(
            lookup::request_schema_name(),
            "runtime/frame/lookup/request"
        );
        assert_eq!(lookup::request_schema_version(), 1);
        assert_eq!(
            FrameLookupResponse::SCHEMA_NAME,
            "runtime/frame/lookup/response"
        );
        assert_eq!(FrameLookupResponse::SCHEMA_VERSION, 1);
        assert_eq!(
            lookup::response_schema_name(),
            "runtime/frame/lookup/response"
        );
        assert_eq!(lookup::response_schema_version(), 1);
        assert_eq!(FrameLookupResponse::busy(), FrameLookupResponse::Busy);
    }

    #[test]
    fn lookup_path_is_stable() {
        assert_eq!(lookup::path(), "runtime/frame/lookup");
    }

    #[test]
    fn topic_paths_are_stable() {
        assert_eq!(super::tree::path(), "runtime/frame/tree");
        assert_eq!(super::r#static::path(), "runtime/frame/static");
        assert_eq!(
            super::data::path(&super::FrameId::new("base_link")),
            "runtime/frame/base_link/data"
        );
    }
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-frame/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
