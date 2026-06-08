use crate::bus::pubsub::Stamped;
use crate::bus::zenoh_typed::TypedSchema;
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

pub fn topic(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> String {
    super::default_profile_topic(bus, component_id, capability_id)
}

pub fn subscriber_builder(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> crate::bus::zenoh_typed::TypedSubscriberBuilder<'_, '_, Stamped<State>> {
    crate::bus::pubsub::subscriber_builder(
        bus,
        &super::default_profile_path(component_id, capability_id),
    )
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh_typed::TypedSchema;

    use super::State;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(State::SCHEMA_NAME, "component/capability/battery");
        assert_eq!(State::SCHEMA_VERSION, 1);
    }
}
