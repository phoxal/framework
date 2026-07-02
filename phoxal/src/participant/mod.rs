//! The participant engine: static metadata traits, contexts, clock, launch contract,
//! `emit-apis`, and the runner.
//!
//! Authoring uses `#[derive(phoxal::Service)]`, `#[derive(phoxal::Driver)]`,
//! `#[derive(phoxal::Tool)]`, or `#[derive(phoxal::Simulator)]` with
//! `#[phoxal::behavior]` (see the crate docs); this module is the
//! runner-facing machinery those macros target.

pub mod clock;
pub mod context;
pub mod emit;
pub mod launch;
pub(crate) mod runner;
mod sd_notify;
pub mod server;
pub mod spec;

pub use clock::{ClockSource, RealClock, TestClock};
pub use context::{SetupContext, ShutdownContext, StepContext, SubscribeBuilder, SubscribeOptions};
pub use emit::{ParticipantMetadata, emit_apis_json, participant_metadata};
pub use launch::{BusProfile, ClockMode, ParticipantLaunch};
pub use runner::{run, run_async, run_with};
pub use server::{ServerOutcome, ServerReply, Snapshot};
pub use spec::{
    ContractUse, DeclaresPublish, DeclaresQuery, DeclaresSubscribe, Direction, IsDriver,
    IsSimulator, IsTool, MissedTick, ParticipantBehavior, ParticipantSpec, StepSchedule,
};

/// Re-exported so authoring code can name logical time without reaching into
/// `bus` (the prelude re-exports this).
pub use crate::bus::LogicalTime;

#[cfg(test)]
mod tests;
