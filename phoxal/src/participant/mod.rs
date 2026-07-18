//! The participant engine: static metadata traits, contexts, clock, launch contract,
//! and the runner.
//!
//! Authoring uses `#[phoxal::service]`, `#[phoxal::driver]`, `#[phoxal::tool]`,
//! or `#[phoxal::simulator]` (with a companion `#[derive(phoxal::Api)]` /
//! `#[derive(phoxal::Config)]`) plus `#[phoxal::behavior]` (see the crate
//! docs); this module is the runner-facing machinery those macros target.

pub mod api;
mod bus_log;
pub mod clock;
pub mod context;
mod heartbeat;
pub mod launch;
mod managed;
pub mod metadata;
#[cfg(feature = "preview-v2")]
mod process_metrics;
#[cfg(not(feature = "preview-v2"))]
#[path = "process_metrics_disabled.rs"]
mod process_metrics;
pub(crate) mod runner;
pub mod scheduler;
mod sd_notify;
pub mod server;
pub mod spec;

pub use api::{
    ApiContractUse, ContractRole, DeclaresAsk, DeclaresPublish, DeclaresServe, DeclaresSubscribe,
    Participant, ParticipantApi, ParticipantConfig, ParticipantLifecycle, Server,
    SetupContextApiExt, SetupContextDriverExt, SetupContextSimulatorExt, SetupContextToolExt,
};
pub use clock::{ClockSource, RealClock, TestClock};
pub use context::{SetupContext, ShutdownContext, StepContext};
pub use launch::{BusProfile, ClockMode, ParticipantLaunch};
pub use managed::ManagedTaskPolicy;
pub use runner::{run, run_async, run_with};
pub use scheduler::{
    AnyStepScheduler, RealScheduler, SchedulerTick, SimulationClockHandle, SimulationScheduler,
    StepScheduler,
};
pub use server::{ServerOutcome, ServerReply, Snapshot};
pub use spec::{IsDriver, IsSimulator, IsTool, MissedTick, StepSchedule, TypedGraphSurface};

/// Re-exported so authoring code can name logical time without reaching into
/// `bus` (the prelude re-exports this).
pub use crate::bus::LogicalTime;

#[cfg(test)]
mod tests;
