//! Scenario authoring and execution surface.
//!
//! A [`Scenario`](crate::scenario::Scenario) author declares one finite
//! simulated experiment through a
//! [`ScenarioPlan`](crate::scenario::ScenarioPlan) and validates its outcome through
//! [`Scenario::verify`](crate::scenario::Scenario::verify) over a
//! [`ScenarioRun`](crate::scenario::ScenarioRun) of typed records and selected native
//! samples.
//!
//! Public identity of a scenario is exactly `scenarios/<StructIdent>`. The
//! filename or module path is intentionally not part of that identity: the
//! registry reports it as diagnostics only.

#[cfg(test)]
mod harness;
mod participant;
mod plan;
mod program;
mod publication;
mod registry;
mod results;
mod trait_def;

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
pub use results::{
    CaptureRecord, CommandReply, EvidenceCollector, ScenarioRun, SealError, TerminalEvidence,
};
// Re-export the canonical native-body wire type from the shared artifact
// format crate so scenario authors never have to import the compiler to
// read simulator terminal evidence.
pub use phoxal_artifact_format::simulation::NativeBodySample;
pub use trait_def::{Scenario, ScenarioBox};

// Re-exports used by the `#[phoxal::scenario]` macro's expansion. Hidden so
// end-users do not reach for them; the macro path is the only public entry.
#[doc(hidden)]
pub mod __macro {
    pub use inventory;
}
