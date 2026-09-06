//! The `supervisor` contract family: the wire vocabulary a supervisor speaks.
//!
//! The framework-owned `phoxal-supervisor` process owns supervisor state and
//! behavior; this module owns only what an answer looks like.
//!
//! Compatibility is owned entirely by the framework train version each binary
//! embeds. The one exception is `connect`, the frozen bootstrap two binaries
//! exchange before they know whether their trains agree: it reports the exact
//! version, and the reader decides.

crate::nodes! {
    family Supervisor;

    bundle;
    command;
    connect;
    info;
    logs;
    snapshot;
    simulation;
    telemetry;
    time_domain;
}

/// The supervisor's execution projection.
///
/// A payload module with no endpoints of its own: the projection is what the
/// `snapshot` node publishes and answers with, so it is declared here beside
/// the family rather than under a node nothing addresses.
pub mod execution;
