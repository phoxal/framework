use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::{TypedPublisherBuilder, TypedSchema, TypedSubscriberBuilder};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contact {
    pub touching: bool,
    pub links: Vec<String>,
}

impl TypedSchema for Contact {
    const SCHEMA_NAME: &'static str = "simulation/robot/contact";
    const SCHEMA_VERSION: u32 = 1;
}

pub const SCHEMA: &str = "simulation/robot/contact";

pub fn path(robot_id: impl AsRef<str>) -> String {
    format!("simulation/robot/{}/contact", robot_id.as_ref())
}

pub fn topic(bus: &crate::bus::Bus, robot_id: impl AsRef<str>) -> String {
    bus.topic(&path(robot_id))
}

pub fn publisher(
    bus: &crate::bus::Bus,
    robot_id: impl AsRef<str>,
) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<Contact>>> {
    crate::bus::pubsub::publisher_builder(bus, &path(robot_id))
}

pub fn subscriber_builder(
    bus: &crate::bus::Bus,
    robot_id: impl AsRef<str>,
) -> TypedSubscriberBuilder<'_, 'static, Stamped<Contact>> {
    crate::bus::pubsub::subscriber_builder(bus, &path(robot_id))
}
