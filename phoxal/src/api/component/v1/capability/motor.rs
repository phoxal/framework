use crate::bus::pubsub::Stamped;
use crate::bus::zenoh_typed::{TypedPublisherBuilder, TypedSchema};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Velocity(Velocity),
    Position(Position),
    Torque(Torque),
}

impl TypedSchema for Command {
    const SCHEMA_NAME: &'static str = "component/capability/motor";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "motor";

pub fn topic(component_id: impl AsRef<str>, capability_id: impl AsRef<str>) -> String {
    super::command_path(component_id, KIND, capability_id)
}

pub fn publisher<'a>(
    bus: &'a crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> crate::bus::Result<TypedPublisherBuilder<'a, 'static, Stamped<Command>>> {
    crate::bus::pubsub::publisher_builder(bus, &topic(component_id, capability_id))
}

pub type Velocity = f32;
pub type Position = f32;
pub type Torque = f32;

#[cfg(test)]
mod tests {
    use crate::bus::zenoh_typed::TypedSchema;

    use super::Command;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Command::SCHEMA_NAME, "component/capability/motor");
        assert_eq!(Command::SCHEMA_VERSION, 1);
    }
}
