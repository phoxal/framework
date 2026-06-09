use crate::bus::zenoh::TypedSchema;
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

crate::bus::topic_leaf! {
    pubsub(robot_id: &str) {
        path: "simulation/robot/{}/contact",
        payload: Contact
    }
}
