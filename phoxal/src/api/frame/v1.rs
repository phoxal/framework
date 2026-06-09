pub const SCHEMA_NAME: &str = "phoxal-api-frame/v1";
pub const SCHEMA_VERSION: u32 = 1;

use std::fmt;

use crate::api::joint::v1::JointId;
use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::{BusyResponse, TypedPublisherBuilder, TypedSchema, TypedSubscriberBuilder};
use serde::{Deserialize, Serialize};

pub const TREE_TOPIC: &str = "runtime/frame/tree";
pub const STATIC_TOPIC: &str = "runtime/frame/static";
pub const DATA_SCHEMA: &str = "runtime/frame/data";
/// Topic template for per-frame transform streams. The `{frame-id}` placeholder
/// is substituted at subscribe time by `data::path(...)`.
pub const FRAME_TRANSFORM_TOPIC_TEMPLATE: &str = "runtime/frame/{frame-id}/data";
pub const LOOKUP_TOPIC: &str = "runtime/frame/lookup";
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

pub mod tree {
    use super::*;

    pub const TOPIC: &str = TREE_TOPIC;

    pub fn topic(bus: &crate::bus::Bus) -> String {
        bus.topic(TOPIC)
    }

    pub fn publisher(
        bus: &crate::bus::Bus,
    ) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<Tree>>> {
        crate::bus::pubsub::publisher_builder(bus, TOPIC)
    }

    pub fn subscriber_builder(
        bus: &crate::bus::Bus,
    ) -> TypedSubscriberBuilder<'_, 'static, Stamped<Tree>> {
        crate::bus::pubsub::subscriber_builder(bus, TOPIC)
    }
}

pub mod r#static {
    use super::*;

    pub const TOPIC: &str = STATIC_TOPIC;

    pub fn topic(bus: &crate::bus::Bus) -> String {
        bus.topic(TOPIC)
    }

    pub fn publisher(
        bus: &crate::bus::Bus,
    ) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<Static>>> {
        crate::bus::pubsub::publisher_builder(bus, TOPIC)
    }

    pub fn subscriber_builder(
        bus: &crate::bus::Bus,
    ) -> TypedSubscriberBuilder<'_, 'static, Stamped<Static>> {
        crate::bus::pubsub::subscriber_builder(bus, TOPIC)
    }
}

pub mod data {
    use super::*;

    pub const SCHEMA: &str = DATA_SCHEMA;

    pub fn path(frame_id: &FrameId) -> String {
        format!("runtime/frame/{frame_id}/data")
    }

    pub fn topic(bus: &crate::bus::Bus, frame_id: &FrameId) -> String {
        bus.topic(&path(frame_id))
    }

    pub fn publisher<'a>(
        bus: &'a crate::bus::Bus,
        frame_id: &FrameId,
    ) -> crate::bus::Result<TypedPublisherBuilder<'a, 'static, Stamped<FrameTransform>>> {
        crate::bus::pubsub::publisher_builder(bus, &path(frame_id))
    }

    pub fn subscriber_builder<'a>(
        bus: &'a crate::bus::Bus,
        frame_id: &FrameId,
    ) -> TypedSubscriberBuilder<'a, 'static, Stamped<FrameTransform>> {
        crate::bus::pubsub::subscriber_builder(bus, &path(frame_id))
    }
}

pub mod lookup {
    use crate::api::frame::v1::{FrameLookupRequest, FrameLookupResponse, LOOKUP_TOPIC};

    pub const TOPIC: &str = LOOKUP_TOPIC;

    pub fn topic(bus: &crate::bus::Bus) -> String {
        bus.topic(TOPIC)
    }

    pub fn get_builder<'a>(
        bus: &'a crate::bus::Bus,
        request: &'a FrameLookupRequest,
    ) -> crate::bus::zenoh::TypedGetBuilder<'a, 'static, FrameLookupResponse> {
        crate::bus::query::get_builder(bus, TOPIC, request)
    }

    pub fn queryable_builder(
        bus: &crate::bus::Bus,
    ) -> crate::bus::Result<
        crate::bus::zenoh::TypedQueryableBuilder<
            '_,
            'static,
            FrameLookupRequest,
            FrameLookupResponse,
        >,
    > {
        crate::bus::query::queryable_builder(bus, TOPIC)
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::{BusyResponse, TypedSchema};

    use crate::api::frame::v1::{
        FrameLookupRequest, FrameLookupResponse, FrameTransform, Static, Tree,
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
            FrameLookupResponse::SCHEMA_NAME,
            "runtime/frame/lookup/response"
        );
        assert_eq!(FrameLookupResponse::SCHEMA_VERSION, 1);
        assert_eq!(FrameLookupResponse::busy(), FrameLookupResponse::Busy);
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
