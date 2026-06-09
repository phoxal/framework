use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::{TypedPublisherBuilder, TypedSchema};
use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, new)]
pub enum Command {
    On,
    Off,
}

impl TypedSchema for Command {
    const SCHEMA_NAME: &'static str = "component/capability/led";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "led";

pub fn topic(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> String {
    super::command_topic(bus, component_id, KIND, capability_id)
}

pub fn publisher(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<Command>>> {
    crate::bus::pubsub::publisher_builder(
        bus,
        &super::command_path(component_id, KIND, capability_id),
    )
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::Command;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Command::SCHEMA_NAME, "component/capability/led");
        assert_eq!(Command::SCHEMA_VERSION, 1);
    }
}
