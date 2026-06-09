use crate::bus::pubsub::Stamped;
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

pub fn topic(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> String {
    super::default_profile_topic(bus, component_id, capability_id)
}

pub fn subscriber_builder(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> crate::bus::zenoh::TypedSubscriberBuilder<'_, '_, Stamped<Sample>> {
    crate::bus::pubsub::subscriber_builder(
        bus,
        &super::default_profile_path(component_id, capability_id),
    )
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
}
