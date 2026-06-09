use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::{TypedPublisherBuilder, TypedSchema, TypedSubscriberBuilder};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Status {
    pub epoch: u64,
    pub step: u64,
    pub time_ns: u64,
    pub dt_ns: u64,
}

impl TypedSchema for Status {
    const SCHEMA_NAME: &'static str = "simulation/status";
    const SCHEMA_VERSION: u32 = 1;
}

pub const TOPIC: &str = "simulation/status";

pub fn topic(bus: &crate::bus::Bus) -> String {
    bus.topic(TOPIC)
}

pub fn publisher(
    bus: &crate::bus::Bus,
) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<Status>>> {
    crate::bus::pubsub::publisher_builder(bus, TOPIC)
}

pub fn subscriber_builder(
    bus: &crate::bus::Bus,
) -> TypedSubscriberBuilder<'_, 'static, Stamped<Status>> {
    crate::bus::pubsub::subscriber_builder(bus, TOPIC)
}
