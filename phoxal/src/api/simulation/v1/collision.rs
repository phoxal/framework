use crate::bus::pubsub::Stamped;
use crate::bus::zenoh_typed::{TypedPublisherBuilder, TypedSchema, TypedSubscriberBuilder};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collision {
    pub collided: bool,
    pub pairs: Vec<[String; 2]>,
}

impl TypedSchema for Collision {
    const SCHEMA_NAME: &'static str = "simulation/robot/collision";
    const SCHEMA_VERSION: u32 = 1;
}

pub const SCHEMA: &str = "simulation/robot/collision";

pub fn path(robot_id: impl AsRef<str>) -> String {
    format!("simulation/robot/{}/collision", robot_id.as_ref())
}

pub fn topic(bus: &crate::bus::Bus, robot_id: impl AsRef<str>) -> String {
    bus.topic(&path(robot_id))
}

pub fn publisher(
    bus: &crate::bus::Bus,
    robot_id: impl AsRef<str>,
) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<Collision>>> {
    crate::bus::pubsub::publisher_builder(bus, &path(robot_id))
}

pub fn subscriber_builder(
    bus: &crate::bus::Bus,
    robot_id: impl AsRef<str>,
) -> TypedSubscriberBuilder<'_, 'static, Stamped<Collision>> {
    crate::bus::pubsub::subscriber_builder(bus, &path(robot_id))
}
