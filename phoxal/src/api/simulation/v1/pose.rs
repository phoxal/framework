use crate::bus::zenoh::TypedSchema;
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

crate::bus::topic_leaf! {
    pubsub(robot_id: &str) {
        path: "simulation/robot/{}/pose",
        payload: Pose
    }
}
