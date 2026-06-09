use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::TypedSchema;
use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub struct Scan {
    #[new(into)]
    detections: Vec<Detection>,
}

impl Scan {
    pub fn detections(&self) -> &[Detection] {
        &self.detections
    }
}

impl TypedSchema for Scan {
    const SCHEMA_NAME: &'static str = "component/capability/mmwave";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "mmwave";

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
) -> crate::bus::zenoh::TypedSubscriberBuilder<'_, '_, Stamped<Scan>> {
    crate::bus::pubsub::subscriber_builder(
        bus,
        &super::default_profile_path(component_id, capability_id),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub struct Detection {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub snr: f32,
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::Scan;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Scan::SCHEMA_NAME, "component/capability/mmwave");
        assert_eq!(Scan::SCHEMA_VERSION, 1);
    }
}
