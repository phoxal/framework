use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::TypedSchema;
use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, new)]
pub struct Audio {
    #[serde(with = "serde_bytes")]
    #[new(into)]
    data: Vec<u8>,
}

impl Audio {
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

impl TypedSchema for Audio {
    const SCHEMA_NAME: &'static str = "component/capability/speaker/audio";
    const SCHEMA_VERSION: u32 = 1;
}

pub const TOPIC_KIND: &str = "speaker/audio";

pub fn path(component_id: impl AsRef<str>, capability_id: impl AsRef<str>) -> String {
    super::super::stream_path(
        component_id,
        TOPIC_KIND,
        capability_id,
        super::super::DATA_STREAM,
    )
}

pub fn topic(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> String {
    bus.topic(&path(component_id, capability_id))
}

pub fn subscriber_builder(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> crate::bus::zenoh::TypedSubscriberBuilder<'_, '_, Stamped<Audio>> {
    crate::bus::pubsub::subscriber_builder(bus, &path(component_id, capability_id))
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::Audio;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Audio::SCHEMA_NAME, "component/capability/speaker/audio");
        assert_eq!(Audio::SCHEMA_VERSION, 1);
    }
}
