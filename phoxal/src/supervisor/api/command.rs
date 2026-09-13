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

crate::endpoints! {
    self: Query<Request, Reply>;
}

use serde::{Deserialize, Serialize};

/// One acknowledged host operation requested from supervisor authority.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    /// Ask the host to restart.
    Reboot,
    /// Ask the host to power off.
    Poweroff,
}

/// Outcome of one acknowledged supervisor command.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CommandOutcome {
    /// The snapshot revision the command was accepted at, so a client can tell
    /// which view of the execution the host acted from.
    Accepted { at_revision: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "schema")]
pub enum Request {
    #[serde(rename = "phoxal/supervisor-control/request/v0")]
    V0 { command: Command },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "schema")]
pub enum Reply {
    #[serde(rename = "phoxal/supervisor-control/reply/v0")]
    V0 { outcome: CommandOutcome },
}
