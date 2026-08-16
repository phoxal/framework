//! Acknowledged operations against supervisor authority.
//!
//! There are exactly two, and both act on the host rather than on the robot
//! graph: reboot and power off. An observe-only supervisor starts nothing and
//! stops nothing, so there is no `restart` and no `stop` here - a client that
//! launched a runtime stops that runtime itself, and a client that did not has
//! no business stopping it through a process that never started it either.
//!
//! What is left is the one thing only the machine running the supervisor can
//! do for a remote operator: cycle its own power. Both requests are guarded by
//! the snapshot revision the operator was looking at when they asked, so a
//! reboot cannot be issued against a view of the robot that has since moved on.
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
    /// Acknowledge the current revision, then ask the host to restart.
    Reboot { expected_revision: u64 },
    /// Acknowledge the current revision, then ask the host to power off.
    Poweroff { expected_revision: u64 },
}

/// Outcome of one acknowledged supervisor command.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CommandOutcome {
    /// The snapshot revision the accepted command was acknowledged against.
    Accepted { at_revision: u64 },
    Rejected { reason: CommandRejection },
}

/// Typed command rejection suitable for caller branching.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CommandRejection {
    /// The operator asked against a snapshot the supervisor has moved past.
    RevisionStale,
    #[serde(other)]
    Unknown,
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
    use phoxal_runtime_contract::wire_schema::{DescribeWire, VariantBody, WireSchema};

    use super::{Command, CommandOutcome, CommandRejection, Reply, Request};

    /// The documents carry their own format tag, independent of the endpoint
    /// key they travel on.
    #[test]
    fn command_documents_round_trip_with_explicit_schema_tags() {
        let request = Request::V0 {
            command: Command::Reboot {
                expected_revision: 17,
            },
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

    /// Both host actions are revision guarded, and both survive the bus codec
    /// under their snake_case spelling.
    #[test]
    fn every_host_action_round_trips_revision_guarded() {
        for command in [
            Command::Reboot {
                expected_revision: 3,
            },
            Command::Poweroff {
                expected_revision: 3,
            },
        ] {
            let encoded = rmp_serde::to_vec_named(&command).expect("command encodes");
            assert_eq!(
                rmp_serde::from_slice::<Command>(&encoded).expect("command decodes"),
                command
            );
        }
        assert_eq!(
            serde_json::to_value(Command::Poweroff {
                expected_revision: 3
            })
            .unwrap()["command"],
            "poweroff"
        );
    }

    /// A rejection reason a peer does not know must decode as `Unknown` rather
    /// than fail the whole reply, and the fallback is part of the declared wire
    /// shape rather than a fact restated here.
    #[test]
    fn command_rejections_round_trip_and_absorb_an_unknown_reason() {
        let encoded =
            rmp_serde::to_vec_named(&CommandRejection::RevisionStale).expect("reason encodes");
        assert_eq!(
            rmp_serde::from_slice::<CommandRejection>(&encoded).expect("reason decodes"),
            CommandRejection::RevisionStale
        );

        let foreign = rmp_serde::to_vec_named("invented_by_a_newer_peer").unwrap();
        assert_eq!(
            rmp_serde::from_slice::<CommandRejection>(&foreign).unwrap(),
            CommandRejection::Unknown
        );

        let rejections = CommandRejection::wire_schema();
        let WireSchema::Enum { variants, .. } = &rejections else {
            panic!("a rejection is a sum type: {rejections:?}");
        };
        assert!(
            variants
                .iter()
                .any(|variant| variant.name == "unknown" && variant.body == VariantBody::Other),
            "the unknown-reason fallback is part of the wire shape: {variants:?}"
        );
    }

    /// The internally tagged documents are declared, not assumed: the derived
    /// shape has to be the shape serde writes.
    #[test]
    fn the_declared_command_shapes_are_the_shapes_serde_writes() {
        let request = Request::V0 {
            command: Command::Reboot {
                expected_revision: 5,
            },
        };
        let json = serde_json::to_value(request).expect("a request serializes");
        assert_eq!(Request::wire_schema().conforms(&json), Ok(()));

        let outcome = CommandOutcome::Rejected {
            reason: CommandRejection::RevisionStale,
        };
        let json = serde_json::to_value(outcome).expect("an outcome serializes");
        assert_eq!(CommandOutcome::wire_schema().conforms(&json), Ok(()));
    }
}
