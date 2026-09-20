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
pub mod harness_support;
mod plan;
mod program;
mod publication;
mod registry;
mod results;
mod trait_def;

pub use plan::{Action, Capture, MAX_PAYLOAD, PlanValidationError, ScenarioPlan, Step, Validity};
pub use program::{PROGRAM_SCHEMA_VERSION, Program, ProgramError, Quantum, ScheduleEntry};
pub use registry::{
    DuplicateLocation, DuplicateScenarioError, PlannedScenario, RegisteredScenario,
    ScenarioDescriptor, ScenarioEntryFn, ScenarioOutcome, ScenarioRegistry, list_scenarios,
};
pub use results::{
    CaptureRecord, CommandReply, EvidenceCollector, ScenarioRun, SealError, StepOutcome,
    TerminalEvidence,
};
// Re-export the canonical native-body wire type so scenario authors can read
// simulator terminal evidence without importing the tool-facing module.
pub use crate::artifact::simulation::NativeBodySample;
pub use trait_def::{Scenario, ScenarioBox};

// Re-exports used by the `#[phoxal::scenario]` macro's expansion. Hidden so
// end-users do not reach for them; the macro path is the only public entry.
#[doc(hidden)]
pub mod __macro {
    pub use inventory;
}

/// Hidden cross-crate surface for generated scenario harnesses.
///
/// The generated `main.rs` the tool writes under
/// `<robot>/.phoxal/generated/scenarios/main.rs` imports this module to
/// drive the case-host control channel. It is intentionally hidden so
/// end-user code never reaches for it directly; it is a small
/// generated-harness entry point, not a place for Cargo, keyring,
/// process spawning, or provisioning (those belong to the tool).
///
/// The tool never sees this module; the harness side never sees the
/// tool. The two communicate over a length-prefixed JSON channel
/// passed in via `PHOXAL_HARNESS_CTL_IN` and `PHOXAL_HARNESS_CTL_OUT`.
#[doc(hidden)]
pub mod __harness {
    pub use crate::scenario::harness_support::{
        Channel, ENV_CTL_IN, ENV_CTL_OUT, HarnessError, HarnessRequest, HarnessResponse,
        MAX_FRAME_BYTES, ScenarioSummary, Verdict, decode_program_envelope,
        encode_program_envelope, run_harness_case,
    };
}
