pub const SCHEMA_NAME: &str = "phoxal-api-joint/v1";
pub const SCHEMA_VERSION: u32 = 1;

use std::fmt;

use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

pub const DATA_SCHEMA: &str = "runtime/joint/data";
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

crate::bus::topic_leaf! {
    pubsub data(joint_id: &JointId) {
        path: "runtime/joint/{}/data",
        payload: JointState
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use crate::api::joint::v1::{JointId, JointState, data};

    #[test]
    fn joint_state_schema_is_stable() {
        assert_eq!(JointState::SCHEMA_NAME, "runtime/joint/data");
        assert_eq!(JointState::SCHEMA_VERSION, 1);
        assert_eq!(data::schema_name(), "runtime/joint/data");
        assert_eq!(data::schema_version(), 1);
    }

    #[test]
    fn data_path_is_stable() {
        assert_eq!(
            data::path(&JointId::new("left_wheel")),
            "runtime/joint/left_wheel/data"
        );
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
