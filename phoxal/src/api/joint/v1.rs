pub const SCHEMA_NAME: &str = "phoxal-api-joint/v1";
pub const SCHEMA_VERSION: u32 = 1;

use std::fmt;

use crate::bus::pubsub::Stamped;
use crate::bus::zenoh_typed::{TypedPublisherBuilder, TypedSchema, TypedSubscriberBuilder};
use serde::{Deserialize, Serialize};

pub const DATA_SCHEMA: &str = "runtime/joint/data";
/// Topic template for per-joint state streams. The `{joint-id}` placeholder is
/// substituted at subscribe time by `data::path(...)`.
pub const JOINT_STATE_TOPIC_TEMPLATE: &str = "runtime/joint/{joint-id}/data";
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct JointId(pub String);

impl JointId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for JointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JointState {
    pub value: f64,
    pub quantity: Quantity,
}

impl TypedSchema for JointState {
    const SCHEMA_NAME: &'static str = DATA_SCHEMA;
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quantity {
    AngleRad,
    LinearM,
}

pub mod data {
    use super::*;

    pub const SCHEMA: &str = DATA_SCHEMA;

    pub fn path(joint_id: &JointId) -> String {
        format!("runtime/joint/{}/data", joint_id)
    }

    pub fn topic(bus: &crate::bus::Bus, joint_id: &JointId) -> String {
        bus.topic(&path(joint_id))
    }

    pub fn publisher<'a>(
        bus: &'a crate::bus::Bus,
        joint_id: &JointId,
    ) -> crate::bus::Result<TypedPublisherBuilder<'a, 'static, Stamped<JointState>>> {
        crate::bus::pubsub::publisher_builder(bus, &path(joint_id))
    }

    pub fn subscriber_builder<'a>(
        bus: &'a crate::bus::Bus,
        joint_id: &JointId,
    ) -> TypedSubscriberBuilder<'a, 'static, Stamped<JointState>> {
        crate::bus::pubsub::subscriber_builder(bus, &path(joint_id))
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh_typed::TypedSchema;

    use crate::api::joint::v1::JointState;

    #[test]
    fn joint_state_schema_is_stable() {
        assert_eq!(JointState::SCHEMA_NAME, "runtime/joint/data");
        assert_eq!(JointState::SCHEMA_VERSION, 1);
    }
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-joint/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
