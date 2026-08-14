//! The `supervisor` contract family: the wire vocabulary a supervisor speaks.
//!
//! This crate owns the vocabulary and nothing else. `phoxald`, the CLI's
//! supervisor daemon, is the sole owner of supervisor *state*: what a snapshot
//! contains, when a process restarts, how long retained logs and telemetry
//! live, and what a command actually does. A type here says only what such an
//! answer looks like on the wire.
//!
//! The family names no collector inside the runtime: process-local facts live
//! in the [`runtime`](crate::api::runtime) family, and the retained views below
//! are a supervisor's replayable projection of them.

pub(crate) mod bundle;
pub(crate) mod command;
pub(crate) mod connect;
pub(crate) mod execution;
pub(crate) mod info;
pub(crate) mod logs;
pub(crate) mod snapshot;
pub(crate) mod telemetry;

phoxal_macros::protocol_fragment_group! {
    fragments {
        bundle;
        command;
        connect;
        info;
        logs;
        snapshot;
        telemetry;
    }
}
