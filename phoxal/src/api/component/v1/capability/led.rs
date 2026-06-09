use crate::bus::zenoh::TypedSchema;
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

crate::bus::topic_leaf! {
    pubsub(component_id: &str, capability_id: &str) {
        path: "component/{}/led/{}/command",
        payload: Command
    }
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

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("status_light", "rgb"),
            "component/status_light/led/rgb/command"
        );
    }
}
