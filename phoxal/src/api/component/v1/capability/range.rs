use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    distance_m: f32,
    limits: Option<Limits>,
    measured_at_ns: Option<u64>,
    quality: Option<SampleQuality>,
    health: SensorHealth,
}

impl Sample {
    pub fn new(distance_m: f32) -> Self {
        Self {
            distance_m,
            limits: None,
            measured_at_ns: None,
            quality: None,
            health: SensorHealth::Nominal,
        }
    }

    pub const fn distance_m(&self) -> f32 {
        self.distance_m
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Limits {
    pub min_m: f32,
    pub max_m: f32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SampleQuality {
    pub valid: bool,
    pub confidence: Option<f32>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorHealth {
    Nominal,
    Degraded,
    Fault,
}

impl TypedSchema for Sample {
    const SCHEMA_NAME: &'static str = "component/capability/range";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "range";

pub fn topic(component_id: impl AsRef<str>, capability_id: impl AsRef<str>) -> String {
    super::default_profile_path(component_id, capability_id)
}

pub fn subscriber_builder(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> crate::bus::zenoh::TypedSubscriberBuilder<'_, '_, Stamped<Sample>> {
    crate::bus::pubsub::subscriber_builder(bus, &topic(component_id, capability_id))
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::Sample;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Sample::SCHEMA_NAME, "component/capability/range");
        assert_eq!(Sample::SCHEMA_VERSION, 1);
    }
}
