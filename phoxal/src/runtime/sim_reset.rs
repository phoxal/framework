//! Simulator reset wire contract.

use crate::bus::zenoh_typed::TypedSchema;
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

/// `simulation/reset` is a command-with-ack: it changes simulator state by
/// starting a new epoch, while request/reply transport is used only to carry
/// the acknowledgement. Keep it out of every `query/` namespace.
pub const TOPIC: &str = "simulation/reset";

pub fn topic(bus: &crate::bus::Bus) -> String {
    bus.topic(TOPIC)
}

pub async fn request(
    bus: &crate::bus::Bus,
    request: &Request,
    retry: &crate::bus::query::Retry,
) -> crate::bus::Result<Option<Response>> {
    crate::bus::query::query(bus, TOPIC, request, retry).await
}

pub fn responder_builder(
    bus: &crate::bus::Bus,
) -> crate::bus::Result<
    crate::bus::zenoh_typed::TypedQueryableBuilder<'_, 'static, Request, Response>,
> {
    crate::bus::query::queryable_builder(bus, TOPIC)
}

pub async fn responder(
    bus: &crate::bus::Bus,
) -> crate::bus::Result<crate::bus::zenoh_typed::TypedQueryable<Request, Response>> {
    crate::bus::query::queryable(bus, TOPIC).await
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh_typed::TypedSchema;

    use super::{Request, Response, TOPIC};

    #[test]
    fn reset_contract_matches_simulator_wire_values() {
        assert_eq!(Request::SCHEMA_NAME, "simulation/reset/request");
        assert_eq!(Request::SCHEMA_VERSION, 1);
        assert_eq!(Response::SCHEMA_NAME, "simulation/reset/response");
        assert_eq!(Response::SCHEMA_VERSION, 1);
        assert_eq!(TOPIC, "simulation/reset");
    }
}
