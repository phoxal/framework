use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::TypedSchema;
use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, new)]
pub struct Frame {
    #[new(into)]
    data: Vec<u8>,
}

impl Frame {
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

impl TypedSchema for Frame {
    const SCHEMA_NAME: &'static str = "component/capability/microphone";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "microphone";

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
) -> crate::bus::zenoh::TypedSubscriberBuilder<'_, '_, Stamped<Frame>> {
    crate::bus::pubsub::subscriber_builder(
        bus,
        &super::default_profile_path(component_id, capability_id),
    )
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::Frame;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Frame::SCHEMA_NAME, "component/capability/microphone");
        assert_eq!(Frame::SCHEMA_VERSION, 1);
    }
}
