use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::{TypedPublisherBuilder, TypedSchema};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    SetVolume(f64),
}

impl TypedSchema for Command {
    const SCHEMA_NAME: &'static str = "component/capability/speaker/command";
    const SCHEMA_VERSION: u32 = 1;
}

pub const TOPIC_KIND: &str = "speaker/command";

pub fn path(component_id: impl AsRef<str>, capability_id: impl AsRef<str>) -> String {
    super::super::stream_path(
        component_id,
        TOPIC_KIND,
        capability_id,
        super::super::COMMAND_STREAM,
    )
}

pub fn topic(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> String {
    bus.topic(&path(component_id, capability_id))
}

pub fn publisher(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<Command>>> {
    crate::bus::pubsub::publisher_builder(bus, &path(component_id, capability_id))
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::Command;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Command::SCHEMA_NAME, "component/capability/speaker/command");
        assert_eq!(Command::SCHEMA_VERSION, 1);
    }
}
