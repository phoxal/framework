use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Request;

impl TypedSchema for Request {
    const SCHEMA_NAME: &'static str = "simulation/reset/request";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Response {
    pub epoch: u64,
}

impl TypedSchema for Response {
    const SCHEMA_NAME: &'static str = "simulation/reset/response";
    const SCHEMA_VERSION: u32 = 1;
}

// `simulation/reset` is a command-with-ack: it changes simulator state by
// starting a new epoch, while request/reply transport is used only to carry
// the acknowledgement. Keep it out of every `query/` namespace.
crate::bus::topic_leaf! {
    query {
        path: "simulation/reset",
        request: Request,
        response: Response
    }
}

pub async fn request(
    bus: &crate::bus::Bus,
    request: &Request,
    retry: &crate::bus::query::Retry,
) -> crate::bus::Result<Option<Response>> {
    crate::bus::query::query(bus, &path(), request, retry).await
}

pub fn responder_builder(
    bus: &crate::bus::Bus,
) -> crate::bus::Result<crate::bus::zenoh::TypedQueryableBuilder<'_, 'static, Request, Response>> {
    queryable_builder(bus)
}

pub async fn responder(
    bus: &crate::bus::Bus,
) -> crate::bus::Result<crate::bus::zenoh::TypedQueryable<Request, Response>> {
    crate::bus::query::queryable(bus, &path()).await
}
