//! Acknowledged operations against supervisor authority.
//!
//! There are exactly two, and both act on the host rather than on the robot
//! graph: reboot and power off. An observe-only supervisor starts nothing and
//! stops nothing, so there is no `restart` and no `stop` here - a client that
//! launched a runtime stops that runtime itself, and a client that did not has
//! no business stopping it through a process that never started it either.
//!
//! What is left is the one thing only the machine running the supervisor can
//! do for a remote operator: cycle its own power. Neither request is fenced by
//! a snapshot revision. Whether a reboot is safe is a fact about the machine
//! and the operator's intent, not about how many times a Ready lease has moved
//! since the operator last looked, so a request here is a plain acknowledged
//! one.
//!
//! The supervisor decides whether a request is allowed; this module owns only
//! the request and reply documents. The schema tags are parse-time format
//! discriminators owned by the documents themselves, so renaming the endpoint
//! does not rename a persisted tag.

use serde::{Deserialize, Serialize};

/// One acknowledged host operation requested from supervisor authority.
#[derive(phoxal_macros::DescribeWire, Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    /// Ask the host to restart.
    Reboot,
    /// Ask the host to power off.
    Poweroff,
}

/// Outcome of one acknowledged supervisor command.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CommandOutcome {
    /// The snapshot revision the command was accepted at, so a client can tell
    /// which view of the execution the host acted from.
    Accepted { at_revision: u64 },
}

#[derive(phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "schema")]
pub enum Request {
    #[serde(rename = "phoxal/supervisor-control/request/v0")]
    V0 { command: Command },
}

#[derive(phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "schema")]
pub enum Reply {
    #[serde(rename = "phoxal/supervisor-control/reply/v0")]
    V0 { outcome: CommandOutcome },
}

#[cfg(test)]
mod tests {
    use phoxal_runtime_contract::wire_schema::DescribeWire;

    use super::{Command, CommandOutcome, Reply, Request};

    /// The documents carry their own format tag, independent of the endpoint
    /// key they travel on.
    #[test]
    fn command_documents_round_trip_with_explicit_schema_tags() {
        let request = Request::V0 {
            command: Command::Reboot,
        };
        let encoded = rmp_serde::to_vec_named(&request).unwrap();
        assert_eq!(rmp_serde::from_slice::<Request>(&encoded).unwrap(), request);
        assert_eq!(
            serde_json::to_value(request).unwrap()["schema"],
            "phoxal/supervisor-control/request/v0"
        );

        let reply = Reply::V0 {
            outcome: CommandOutcome::Accepted { at_revision: 18 },
        };
        let encoded = rmp_serde::to_vec_named(&reply).unwrap();
        assert_eq!(rmp_serde::from_slice::<Reply>(&encoded).unwrap(), reply);
        assert_eq!(
            serde_json::to_value(reply).unwrap()["schema"],
            "phoxal/supervisor-control/reply/v0"
        );
    }

    /// Both host actions survive the bus codec under their snake_case
    /// spelling, and neither carries anything beyond which action it is.
    #[test]
    fn every_host_action_round_trips() {
        for command in [Command::Reboot, Command::Poweroff] {
            let encoded = rmp_serde::to_vec_named(&command).expect("command encodes");
            assert_eq!(
                rmp_serde::from_slice::<Command>(&encoded).expect("command decodes"),
                command
            );
        }
        assert_eq!(
            serde_json::to_value(Command::Poweroff).unwrap()["command"],
            "poweroff"
        );
    }

    /// The internally tagged documents are declared, not assumed: the derived
    /// shape has to be the shape serde writes.
    #[test]
    fn the_declared_command_shapes_are_the_shapes_serde_writes() {
        let request = Request::V0 {
            command: Command::Reboot,
        };
        let json = serde_json::to_value(request).expect("a request serializes");
        assert_eq!(Request::wire_schema().conforms(&json), Ok(()));

        let outcome = CommandOutcome::Accepted { at_revision: 5 };
        let json = serde_json::to_value(outcome).expect("an outcome serializes");
        assert_eq!(CommandOutcome::wire_schema().conforms(&json), Ok(()));
    }
}
