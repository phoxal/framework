//! The runtime engine: static metadata traits, contexts, clock, launch contract,
//! `emit-apis`, and the runner.
//!
//! Authoring uses `#[derive(phoxal::Runtime)]` + `#[phoxal::runtime]` (see the
//! crate docs); this module is the runner-facing machinery those macros target.

pub mod clock;
pub mod context;
pub mod emit;
pub mod launch;
mod runner;
pub mod server;
pub mod spec;

pub use clock::{ClockSource, RealClock, TestClock};
pub use context::{SetupContext, ShutdownContext, StepContext, SubscribeBuilder};
pub use emit::{RuntimeMetadata, emit_apis_json, runtime_metadata};
pub use launch::{BusProfile, ClockMode, ParticipantLaunch};
pub use runner::{run, run_async, run_with};
pub use server::{ServerOutcome, ServerReply, Snapshot};
pub use spec::{
    ContractUse, Declares, Direction, MissedTick, RuntimeBehavior, RuntimeFields, StepSchedule,
};

/// Re-exported so authoring code can name logical time without reaching into
/// `bus` (the prelude re-exports this).
pub use crate::bus::LogicalTime;

#[cfg(test)]
mod tests;
