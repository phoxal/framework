use crate::bus::zenoh::TypedSchema;
use derive_new::new;
use serde::{Deserialize, Serialize};

/// Raw angular velocity sample in the sensor-local frame in rad/s.
///
/// This payload does not guarantee zero-bias removal or rest-state filtering.
/// Small non-zero readings while stationary are valid unless a specific producer
/// documents additional normalization.
#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub struct Sample {
    angular_velocity: [f32; 3],
}

impl Sample {
    pub const fn angular_velocity(&self) -> &[f32; 3] {
        &self.angular_velocity
    }
}

impl TypedSchema for Sample {
    const SCHEMA_NAME: &'static str = "component/capability/gyroscope";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "gyroscope";

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
        assert_eq!(Sample::SCHEMA_NAME, "component/capability/gyroscope");
        assert_eq!(Sample::SCHEMA_VERSION, 1);
    }

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("imu_board", "gyro"),
            "component/imu_board/gyro/profile/default"
        );
    }
}
