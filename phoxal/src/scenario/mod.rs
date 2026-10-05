//! Scenario authoring and execution surface.
//!
//! Standalone Rust executables construct typed plans and execute them through
//! `cargo phoxal scenario`. Ordinary Rust tests do not require a scenario host.

mod fixture;
pub mod fixture_protocol;
mod plan;
mod program;
mod publication;
mod results;

pub use fixture::{
    CompletedRun, NoReply, Observation, ObservationOperation, Plan, ReplyOutcome, ReplyTicket,
    SendOperation, Simulation,
};

pub use plan::CapturePolicy;
pub(crate) use plan::{Action, Capture, Step, Validity};
pub(crate) use program::{Program, Quantum, ScheduleEntry};
pub(crate) use results::{
    CaptureRecord, CommandReply, EvidenceCollector, ScenarioRun, StepOutcome,
};
// Re-export the canonical native-body wire type so scenario authors can read
// simulator terminal evidence without importing the tool-facing module.
pub use crate::artifact::simulation::NativeBodySample;
pub mod plan_support {
    pub use super::plan::{Action, Capture, Step, Validity};
    pub use super::program::{Program, Quantum};
    pub use super::results::{
        CaptureRecord, CapturedObservation, CommandReply, EvidenceCollector, ScenarioRun,
        StepOutcome,
    };
}
