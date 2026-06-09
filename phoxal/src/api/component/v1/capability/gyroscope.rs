use crate::bus::pubsub::Stamped;
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

    use super::Sample;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Sample::SCHEMA_NAME, "component/capability/gyroscope");
        assert_eq!(Sample::SCHEMA_VERSION, 1);
    }
}
