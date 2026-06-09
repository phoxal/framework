use crate::bus::zenoh::TypedSchema;
use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub struct Sample {
    magnetic_field: [f32; 3],
}

impl Sample {
    pub const fn magnetic_field(&self) -> &[f32; 3] {
        &self.magnetic_field
    }
}

impl TypedSchema for Sample {
    const SCHEMA_NAME: &'static str = "component/capability/magnetometer";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "magnetometer";

crate::bus::topic_leaf! {
    pubsub(component_id: &str, capability_id: &str) {
        path: "component/{}/{}/profile/default",
        payload: Sample
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::Sample;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Sample::SCHEMA_NAME, "component/capability/magnetometer");
        assert_eq!(Sample::SCHEMA_VERSION, 1);
    }

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("imu_board", "mag"),
            "component/imu_board/mag/profile/default"
        );
    }
}
