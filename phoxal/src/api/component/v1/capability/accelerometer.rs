use crate::bus::zenoh::TypedSchema;
use derive_new::new;
use serde::{Deserialize, Serialize};

/// Raw accelerometer sample in the sensor-local frame in m/s^2.
///
/// This payload does not guarantee gravity compensation, zero-bias removal,
/// or rest-state filtering. Small non-zero readings while stationary are valid
/// unless a specific producer documents additional normalization.
#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub struct Sample {
    linear_acceleration: [f32; 3],
}

impl Sample {
    pub const fn linear_acceleration(&self) -> &[f32; 3] {
        &self.linear_acceleration
    }
}

impl TypedSchema for Sample {
    const SCHEMA_NAME: &'static str = "component/capability/accelerometer";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "accelerometer";

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
        assert_eq!(Sample::SCHEMA_NAME, "component/capability/accelerometer");
        assert_eq!(Sample::SCHEMA_VERSION, 1);
    }

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("imu_board", "accel"),
            "component/imu_board/accel/profile/default"
        );
    }
}
