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
        assert_eq!(Sample::SCHEMA_NAME, "component/capability/range");
        assert_eq!(Sample::SCHEMA_VERSION, 1);
    }

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("front_tof", "range"),
            "component/front_tof/range/profile/default"
        );
    }
}
