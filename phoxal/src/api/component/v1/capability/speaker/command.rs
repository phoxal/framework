use crate::bus::zenoh::TypedSchema;
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

pub const KIND: &str = "speaker/command";

crate::bus::topic_leaf! {
    pubsub(component_id: &str, capability_id: &str) {
        path: "component/{}/speaker/command/{}/command",
        payload: Command
    }
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

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("speaker", "volume"),
            "component/speaker/speaker/command/volume/command"
        );
    }
}
