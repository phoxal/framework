use crate::bus::zenoh::TypedSchema;
use derive_new::new;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, new)]
pub struct State {
    voltage_v: f64,
    current_a: f64,
    percentage: f32,
}

impl State {
    pub const fn voltage_v(&self) -> f64 {
        self.voltage_v
    }

    pub const fn current_a(&self) -> f64 {
        self.current_a
    }

    pub const fn percentage(&self) -> f32 {
        self.percentage
    }
}

impl TypedSchema for State {
    const SCHEMA_NAME: &'static str = "component/capability/battery";
    const SCHEMA_VERSION: u32 = 1;
}

pub const KIND: &str = "battery";

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
        assert_eq!(State::SCHEMA_NAME, "component/capability/battery");
        assert_eq!(State::SCHEMA_VERSION, 1);
    }

    #[test]
    fn path_is_stable() {
        assert_eq!(
            super::path("power_board", "main_battery"),
            "component/power_board/main_battery/profile/default"
        );
    }
}
