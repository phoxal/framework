use crate::bus::zenoh::TypedSchema;
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

crate::bus::topic_leaf! {
    pubsub {
        path: "simulation/status",
        payload: Status
    }
}
