pub const SCHEMA_NAME: &str = "phoxal-api-power/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

pub const COMMAND_TOPIC: &str = "runtime/power/command";
pub const STATE_TOPIC: &str = "runtime/power/state";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Poweroff,
    Reboot,
}

impl TypedSchema for Command {
    const SCHEMA_NAME: &'static str = "runtime/power/command";
    const SCHEMA_VERSION: u32 = 1;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub requested: Option<Command>,
    pub status: Status,
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Idle,
    Accepted,
    Rejected(RejectedReason),
    Failed(FailedReason),
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectedReason {
    SupervisorUnavailable,
    SupervisorReturnedHttp { code: u16 },
}

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailedReason {
    SupervisorTransport,
}

impl TypedSchema for State {
    const SCHEMA_NAME: &'static str = "runtime/power/state";
    const SCHEMA_VERSION: u32 = 1;
}

pub mod command {
    use super::Command;
    use crate::bus::pubsub::Stamped;
    use crate::bus::zenoh::{TypedPublisherBuilder, TypedSubscriberBuilder};

    pub const TOPIC: &str = super::COMMAND_TOPIC;

    pub fn topic(bus: &crate::bus::Bus) -> String {
        bus.topic(TOPIC)
    }

    pub fn publisher(
        bus: &crate::bus::Bus,
    ) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<Command>>> {
        crate::bus::pubsub::publisher_builder(bus, TOPIC)
    }

    pub fn subscriber_builder(
        bus: &crate::bus::Bus,
    ) -> TypedSubscriberBuilder<'_, 'static, Stamped<Command>> {
        crate::bus::pubsub::subscriber_builder(bus, TOPIC)
    }
}

pub mod state {
    use super::State;
    use crate::bus::pubsub::Stamped;
    use crate::bus::zenoh::{TypedPublisherBuilder, TypedSubscriberBuilder};

    pub const TOPIC: &str = super::STATE_TOPIC;

    pub fn topic(bus: &crate::bus::Bus) -> String {
        bus.topic(TOPIC)
    }

    pub fn publisher(
        bus: &crate::bus::Bus,
    ) -> crate::bus::Result<TypedPublisherBuilder<'_, 'static, Stamped<State>>> {
        crate::bus::pubsub::publisher_builder(bus, TOPIC)
    }

    pub fn subscriber_builder(
        bus: &crate::bus::Bus,
    ) -> TypedSubscriberBuilder<'_, 'static, Stamped<State>> {
        crate::bus::pubsub::subscriber_builder(bus, TOPIC)
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, State};
    use crate::bus::zenoh::TypedSchema;

    #[test]
    fn command_contract_schema_is_stable() {
        assert_eq!(Command::SCHEMA_NAME, "runtime/power/command");
        assert_eq!(Command::SCHEMA_VERSION, 1);
    }

    #[test]
    fn state_contract_schema_is_stable() {
        assert_eq!(State::SCHEMA_NAME, "runtime/power/state");
        assert_eq!(State::SCHEMA_VERSION, 1);
    }
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-api-power/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
