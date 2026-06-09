use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub engaged: bool,
}

impl TypedSchema for State {
    const SCHEMA_NAME: &'static str = "component/capability/emergency_stop";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "emergency_stop";

crate::bus::topic_leaf! {
    pubsub(component_id: &str, capability_id: &str) {
        path: "component/{}/{}/profile/default",
        payload: State
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::State;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(State::SCHEMA_NAME, "component/capability/emergency_stop");
        assert_eq!(State::SCHEMA_VERSION, 1);
    }

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("safety_panel", "estop"),
            "component/safety_panel/estop/profile/default"
        );
    }
}
