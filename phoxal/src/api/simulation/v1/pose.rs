use crate::bus::pubsub::Stamped;
use crate::bus::zenoh_typed::{TypedPublisherBuilder, TypedSchema, TypedSubscriberBuilder};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pose {
    pub frame_id: String,
    pub translation_m: [f64; 3],
    pub rotation_xyzw: [f64; 4],
}

impl TypedSchema for Pose {
    const SCHEMA_NAME: &'static str = "simulation/robot/pose";
    const SCHEMA_VERSION: u32 = 1;
}

pub const SCHEMA: &str = "simulation/robot/pose";

pub fn path(robot_id: impl AsRef<str>) -> String {
    format!("simulation/robot/{}/pose", robot_id.as_ref())
}

pub fn topic(bus: &crate::bus::Bus, robot_id: impl AsRef<str>) -> String {
    bus.topic(&path(robot_id))
}

pub fn publisher(
    bus: &crate::bus::Bus,
    robot_id: impl AsRef<str>,
) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<Pose>>> {
    crate::bus::pubsub::publisher_builder(bus, &path(robot_id))
}

pub fn subscriber_builder(
    bus: &crate::bus::Bus,
    robot_id: impl AsRef<str>,
) -> TypedSubscriberBuilder<'_, 'static, Stamped<Pose>> {
    crate::bus::pubsub::subscriber_builder(bus, &path(robot_id))
}
