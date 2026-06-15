use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Request;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Response {
    pub epoch: u64,
}

// `simulation/reset` is a command-with-ack: it changes simulator state by
// starting a new epoch, while request/reply transport is used only to carry
// the acknowledgement. Keep it out of every `query/` namespace.
