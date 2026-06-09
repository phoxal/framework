use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::TypedSchema;
use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, new)]
pub struct Sample {
    ticks: i64,
}

impl Sample {
    pub const fn ticks(&self) -> i64 {
        self.ticks
    }
}

impl TypedSchema for Sample {
    const SCHEMA_NAME: &'static str = "component/capability/encoder";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "encoder";

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
        assert_eq!(Sample::SCHEMA_NAME, "component/capability/encoder");
        assert_eq!(Sample::SCHEMA_VERSION, 1);
    }
}
