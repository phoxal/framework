use crate::bus::zenoh::TypedSchema;
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

crate::bus::topic_leaf! {
    pubsub command(component_id: &str, capability_kind: &str, capability_id: &str) {
        path: "component/{}/{}/{}/command",
        payload: Command
    }
}

pub type Velocity = f32;
pub type Position = f32;
pub type Torque = f32;

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::{Command, command};

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(Command::SCHEMA_NAME, "component/capability/motor");
        assert_eq!(Command::SCHEMA_VERSION, 1);
        assert_eq!(command::schema_name(), "component/capability/motor");
        assert_eq!(command::schema_version(), 1);
    }

    #[test]
    fn command_path_is_stable() {
        assert_eq!(
            command::path("base", "motor", "left_wheel"),
            "component/base/motor/left_wheel/command"
        );
    }
}
