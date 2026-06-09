use crate::bus::zenoh::TypedSchema;
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

crate::bus::topic_leaf! {
    pubsub(robot_id: &str) {
        path: "simulation/robot/{}/collision",
        payload: Collision
    }
}
