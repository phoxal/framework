use crate::bus::pubsub::Stamped;
use crate::bus::zenoh_typed::TypedSchema;
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

pub fn topic(component_id: impl AsRef<str>, capability_id: impl AsRef<str>) -> String {
    super::default_profile_path(component_id, capability_id)
}

pub fn subscriber_builder(
    bus: &crate::bus::Bus,
    component_id: impl AsRef<str>,
    capability_id: impl AsRef<str>,
) -> crate::bus::zenoh_typed::TypedSubscriberBuilder<'_, '_, Stamped<State>> {
    crate::bus::pubsub::subscriber_builder(bus, &topic(component_id, capability_id))
}

#[cfg(test)]
mod tests {
    use crate::bus::zenoh_typed::TypedSchema;

    use super::State;

    #[test]
    fn schema_contract_does_not_drift() {
        assert_eq!(State::SCHEMA_NAME, "component/capability/emergency_stop");
        assert_eq!(State::SCHEMA_VERSION, 1);
    }
}
