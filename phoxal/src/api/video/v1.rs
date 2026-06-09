pub const SCHEMA_NAME: &str = "phoxal-api-video/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::{BusyResponse, TypedPublisherBuilder, TypedSchema, TypedSubscriberBuilder};
use serde::{Deserialize, Serialize};

pub const OPEN_TOPIC: &str = "runtime/video/open";
pub const STREAM_TOPIC_PREFIX: &str = "runtime/video/stream";
/// Topic template for per-stream video events. The `{stream-id}` placeholder is
/// substituted at subscribe time by `stream::path(...)`.
pub const STREAM_TOPIC_TEMPLATE: &str = "runtime/video/stream/{stream-id}";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Auto,
    P144,
    P240,
    P360,
    P480,
    P720,
    P1080,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenRequest {
    /// Source camera as `component_id.capability_id` (`CapabilityRef` display form).
    pub source: String,
    pub quality: Quality,
}

impl OpenRequest {
    pub fn new(source: impl Into<String>, quality: Quality) -> Self {
        Self {
            source: source.into(),
            quality,
        }
    }
}

impl TypedSchema for OpenRequest {
    const SCHEMA_NAME: &'static str = "runtime/video/open/request";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenResponse {
    Ok {
        stream_id: String,
        format: StreamFormat,
    },
    UnknownSource,
    Unavailable(UnavailableReason),
    Busy,
}

impl TypedSchema for OpenResponse {
    const SCHEMA_NAME: &'static str = "runtime/video/open/response";
    const SCHEMA_VERSION: u32 = 1;
}

impl BusyResponse for OpenResponse {
    fn busy() -> Self {
        Self::Busy
    }
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    NoCamerasAvailable,
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
    H264,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamFormat {
    pub codec: Codec,
    pub width_px: u32,
    pub height_px: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StreamEvent {
    Opened { format: StreamFormat },
    Packet(StreamPacket),
    Reconfigured { format: StreamFormat },
    End { reason: EndReason },
}

impl TypedSchema for StreamEvent {
    const SCHEMA_NAME: &'static str = "runtime/video/stream";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamPacket {
    pub sequence: u64,
    pub captured_at_ns: u64,
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    SourceUnavailable,
    IdleTimeout,
    RuntimeStopping,
    RejectedByPolicy,
    Released,
}

pub mod open {
    pub use crate::api::video::v1::{
        OPEN_TOPIC as TOPIC, OpenRequest as Request, OpenResponse as Response, Quality,
    };

    pub fn topic(bus: &crate::bus::Bus) -> String {
        bus.topic(TOPIC)
    }

    pub fn get_builder<'a>(
        bus: &'a crate::bus::Bus,
        request: &'a Request,
    ) -> crate::bus::zenoh::TypedGetBuilder<'a, 'static, Response> {
        crate::bus::query::get_builder(bus, TOPIC, request)
    }

    pub fn queryable_builder(
        bus: &crate::bus::Bus,
    ) -> crate::bus::Result<crate::bus::zenoh::TypedQueryableBuilder<'_, 'static, Request, Response>>
    {
        crate::bus::query::queryable_builder(bus, TOPIC)
    }

    pub async fn query(
        bus: &crate::bus::Bus,
        request: &Request,
        retry: &crate::bus::query::Retry,
    ) -> crate::bus::Result<Option<Response>> {
        crate::bus::query::query(bus, TOPIC, request, retry).await
    }
}

pub mod stream {
    use super::*;

    pub use crate::api::video::v1::{
        Codec, EndReason, STREAM_TOPIC_PREFIX as TOPIC_PREFIX, StreamEvent as Event,
        StreamFormat as Format, StreamPacket as Packet,
    };

    pub fn path(stream_id: impl AsRef<str>) -> String {
        format!("{}/{}", TOPIC_PREFIX, stream_id.as_ref())
    }

    pub fn topic(bus: &crate::bus::Bus, stream_id: impl AsRef<str>) -> String {
        bus.topic(&path(stream_id))
    }

    pub fn publisher(
        bus: &crate::bus::Bus,
        stream_id: impl AsRef<str>,
    ) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<Event>>> {
        crate::bus::pubsub::publisher_builder(bus, &path(stream_id))
    }

    pub fn subscriber_builder(
        bus: &crate::bus::Bus,
        stream_id: impl AsRef<str>,
    ) -> TypedSubscriberBuilder<'_, 'static, Stamped<Event>> {
        crate::bus::pubsub::subscriber_builder(bus, &path(stream_id))
    }
}

#[cfg(test)]
mod tests {
    use super::{OpenRequest, OpenResponse, StreamEvent};
    use crate::bus::zenoh::{BusyResponse, TypedSchema};

    #[test]
    fn schema_names_and_versions_do_not_drift() {
        assert_eq!(OpenRequest::SCHEMA_NAME, "runtime/video/open/request");
        assert_eq!(OpenRequest::SCHEMA_VERSION, 1);
        assert_eq!(OpenResponse::SCHEMA_NAME, "runtime/video/open/response");
        assert_eq!(OpenResponse::SCHEMA_VERSION, 1);
        assert_eq!(StreamEvent::SCHEMA_NAME, "runtime/video/stream");
        assert_eq!(StreamEvent::SCHEMA_VERSION, 1);
    }

    #[test]
    fn open_response_busy_uses_busy_variant() {
        assert_eq!(OpenResponse::busy(), OpenResponse::Busy);
    }
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-video/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
