//! Scenario authoring and execution surface.
//!
//! A [`Scenario`](crate::scenario::Scenario) author declares one finite
//! simulated experiment through a
//! [`ScenarioPlan`](crate::scenario::ScenarioPlan) (P2 will land the
//! plan/action/step types) and validates its outcome through
//! [`Scenario::verify`](crate::scenario::Scenario::verify) over a
//! [`ScenarioRun`](crate::scenario::ScenarioRun) of typed records and
//! selected native samples (P3 will land the run types).
//!
//! The trait, registry, and case-host protocol skeleton ship in P1 so the
//! `cargo phoxal simulation scenario list` and `run` commands have a
//! compile-and-dispatch pipeline to land against before P2-P3 add the
//! behaviour.
//!
//! Public identity of a scenario is exactly `scenarios/<StructIdent>`. The
//! filename or module path is intentionally not part of that identity: the
//! registry reports it as diagnostics only.

mod harness;
mod participant;
mod plan;
mod program;
mod publication;
mod registry;
mod results;
mod trait_def;

pub use harness::{HarnessError, HarnessRun, run_harness};
pub use participant::{
    FixtureError, FixtureMetadata, FixtureParticipant, HOST_DEADLINE_TICKS, SIM_DEADLINE_TICKS,
    StepOutcome,
};
pub use plan::{Action, Capture, MAX_PAYLOAD, PlanValidationError, ScenarioPlan, Step, Validity};
pub use program::{PROGRAM_SCHEMA_VERSION, Program, ProgramError, Quantum, ScheduleEntry};
pub use registry::{
    DuplicateLocation, DuplicateScenarioError, PlannedScenario, RegisteredScenario,
    ScenarioDescriptor, ScenarioEntryFn, ScenarioOutcome, ScenarioRegistry, list_scenarios,
};
pub use results::{CaptureRecord, CommandReply, EvidenceCollector, ScenarioRun, SealError};
pub use trait_def::Scenario;

// Re-exports used by the `#[phoxal::scenario]` macro's expansion. Hidden so
// end-users do not reach for them; the macro path is the only public entry.
#[doc(hidden)]
pub mod __macro {
    pub use inventory;
}
