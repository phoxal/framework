//! Scenario authoring and execution surface.
//!
//! Ordinary Rust tests construct a finite typed plan, execute it through the
//! command-scoped run host, and use ordinary Rust assertions over the returned
//! evidence.

mod fixture;
#[doc(hidden)]
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
#[doc(hidden)]
pub mod __internal {
    pub use super::plan::{Action, Capture, Step, Validity};
    pub use super::program::{Program, Quantum};
    pub use super::results::{
        CaptureRecord, CapturedObservation, CommandReply, EvidenceCollector, ScenarioRun,
        StepOutcome,
    };
}
