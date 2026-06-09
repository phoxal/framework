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

crate::bus::topic_leaf! {
    pubsub(component_id: &str, capability_id: &str) {
        path: "component/{}/{}/profile/default",
        payload: Scan
    }
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

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("radar", "mmwave"),
            "component/radar/mmwave/profile/default"
        );
    }
}
