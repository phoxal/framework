//! Private supervisor-to-router bootstrap contract.
//!
//! The router reports exactly one result on an inherited file descriptor after
//! Zenoh has either bound every requested listener or failed. Standard output
//! and standard error remain ordinary human-readable process logs.

use serde::{Deserialize, Serialize};

/// Version-zero result written once to the router readiness descriptor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RouterReadyV0 {
    /// Zenoh opened successfully and bound the exact local endpoint.
    Ready {
        /// The mandatory project-local participant endpoint.
        local_endpoint: String,
        /// Every listener active in the opened router, including authored
        /// external listeners retained alongside the local endpoint.
        listeners: Vec<String>,
    },
    /// Zenoh could not open the requested router configuration.
    Failed {
        /// Bounded diagnostic suitable for surfacing as startup failure.
        message: String,
    },
}
