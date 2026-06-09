use crate::bus::zenoh::TypedSchema;
use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSystem {
    #[default]
    Local,
    Wgs84,
}

#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub struct Sample {
    latitude: f64,
    longitude: f64,
    altitude: f64,
    position_covariance: [f64; 9],
}

impl Sample {
    pub const fn latitude(&self) -> f64 {
        self.latitude
    }

    pub const fn longitude(&self) -> f64 {
        self.longitude
    }

    pub const fn altitude(&self) -> f64 {
        self.altitude
    }

    pub const fn position_covariance(&self) -> &[f64; 9] {
        &self.position_covariance
    }
}

impl TypedSchema for Sample {
    const SCHEMA_NAME: &'static str = "component/capability/gnss";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "gnss";

crate::bus::topic_leaf! {
    pubsub(component_id: &str, capability_id: &str) {
        path: "component/{}/{}/profile/default",
        payload: Sample
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::{CoordinateSystem, Sample};

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Sample::SCHEMA_NAME, "component/capability/gnss");
        assert_eq!(Sample::SCHEMA_VERSION, 1);
    }

    #[test]
    fn coordinate_system_defaults_to_local() {
        assert_eq!(CoordinateSystem::default(), CoordinateSystem::Local);
    }

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("gps", "gnss"),
            "component/gps/gnss/profile/default"
        );
    }
}
