//! The participant engine: static metadata traits, contexts, clock, launch contract,
//! and the runner.
//!
//! Authoring uses a role attribute on a unit marker, an optional
//! `#[derive(phoxal::Config)]`, and an ordinary `Participant` implementation.

pub mod api;
mod bus_log;
pub mod clock;
pub mod context;
pub mod launch;
mod managed;
pub mod metadata;
pub(crate) mod runner;
mod runtime_performance;
pub mod scheduler;
pub mod server;
pub mod spec;

pub use api::{Participant, ParticipantConfig, ParticipantSpec};
pub use clock::{
    BootId, ClockReading, ClockSource, ExecutionOrigin, RealClock, TestClock, TimeUnsynchronized,
};
pub use context::{ResetContext, SetupContext, StepContext};
pub use launch::{BusProfile, ClockMode, ParticipantLaunch};
pub use managed::ManagedTaskPolicy;
pub use runner::{run, run_async, run_with};
pub use scheduler::{
    AnyStepScheduler, RealScheduler, SchedulerTick, SimulationClockHandle, SimulationScheduler,
    StepScheduler,
};
pub use server::{ServerOutcome, ServerReply};
pub use spec::{MissedTick, StepSchedule};

#[cfg(test)]
mod config_tests;
#[cfg(test)]
mod tests;
