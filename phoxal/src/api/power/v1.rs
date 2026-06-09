pub const SCHEMA_NAME: &str = "phoxal-api-power/v1";
pub const SCHEMA_VERSION: u32 = 1;

use crate::bus::zenoh::TypedSchema;
use serde::{Deserialize, Serialize};

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

crate::bus::topic_leaf! {
    pubsub command {
        path: "runtime/power/command",
        payload: Command
    }
}

crate::bus::topic_leaf! {
    pubsub state {
        path: "runtime/power/state",
        payload: State
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

    #[test]
    fn topic_paths_are_stable() {
        assert_eq!(super::command::path(), "runtime/power/command");
        assert_eq!(super::state::path(), "runtime/power/state");
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
