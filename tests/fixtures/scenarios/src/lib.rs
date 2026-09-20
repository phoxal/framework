//! SDK-only scenario authoring fixture.
//!
//! This crate exercises `phoxal::scenario` without depending on the
//! project compiler, the simulator, the supervisor, or any service
//! package. It exists to prove that:
//!
//! - A scenario author can build a `ScenarioPlan` purely from the SDK.
//! - The plan survives `validate_for_quantum` against the probed
//!   quantum of any controlled scene (not just the rover's 2 ms).
//! - A `Program::normalize` produces a wire-stable envelope for that
//!   plan without touching the compiler.
//! - Two non-rover quanta (here, 5 ms and 10 ms) round-trip against
//!   the same plan and yield the expected transition counts.
//!
//! Used as the "SDK-only scenario fixture" the migration plan §12
//! refers to.

#![deny(unsafe_code)]

use std::time::Duration;

use phoxal::port::{PortKind, PortSignature};
use phoxal::scenario::{
    Action, Capture, Program, Quantum, ScenarioPlan, ScheduleEntry, Step, Validity,
};

fn motion_setpoint_sig() -> PortSignature {
    PortSignature::new(
        "motion/cmd",
        "phoxal.motion",
        "Set",
        PortKind::Setpoint,
        "SetpointRequest",
        "SetpointReply",
    )
}

fn motion_state_sig() -> PortSignature {
    PortSignature::new(
        "motion/state",
        "phoxal.motion",
        "State",
        PortKind::State,
        "State",
        "State",
    )
}

/// One reusable plan covering both a 5 ms scene and a 10 ms scene.
/// Two-second duration fits both quanta and exercises four authored
/// actions whose boundaries fall within `[0, transitions)`.
#[expect(
    clippy::expect_used,
    reason = "fixed fixture literals must remain valid SDK examples"
)]
pub fn canonical_plan() -> ScenarioPlan {
    ScenarioPlan::with_steps(
        "scene-fixture/canonical",
        Duration::from_secs(2),
        vec![
            Step::new("drive", 0, drive_setpoint(1)),
            Step::new("observe", 1, state_capture()),
            Step::new("drive-2", 2, drive_setpoint(2)),
            Step::new("observe-2", 3, state_capture()),
        ],
        vec![Capture::state("pose", motion_state_sig()).expect("capture accepts state signature")],
    )
    .expect("plan has shape-valid steps for a 2 s duration")
}

#[expect(
    clippy::expect_used,
    reason = "fixed fixture literals must remain valid SDK examples"
)]
fn drive_setpoint(payload: u8) -> Action {
    Action::setpoint(
        "motion",
        motion_setpoint_sig(),
        vec![payload],
        Validity::Permanent,
    )
    .expect("setpoint accepts a non-empty instance and a Setpoint kind")
}

#[expect(
    clippy::expect_used,
    reason = "fixed fixture literals must remain valid SDK examples"
)]
fn state_capture() -> Action {
    // The boundary entry is a typed capture; use an empty payload
    // because capture boundaries carry no action data here.
    Action::withdraw("motion", motion_state_sig()).expect("withdraw accepts state signature")
}

/// Validate the canonical plan against two non-rover quanta. Both
/// quanta are non-rover (the rover is 2 ms); the plan must satisfy
/// alignment and boundary-range against either, proving the SDK no
/// longer hard-codes the rover quantum.
#[expect(
    clippy::expect_used,
    reason = "fixed fixture literals must remain valid SDK examples"
)]
pub fn validate_against_five_ms(plan: &ScenarioPlan) {
    let quantum = Quantum::from_micros(5_000).expect("5 ms is positive");
    // 2 s at 5 ms = 400 transitions; boundaries 0..=3 are inside.
    assert_eq!(plan.transition_count(quantum), Some(400));
    plan.validate_for_quantum(quantum)
        .expect("canonical plan must validate at 5 ms quantum");
}

#[expect(
    clippy::expect_used,
    reason = "fixed fixture literals must remain valid SDK examples"
)]
pub fn validate_against_ten_ms(plan: &ScenarioPlan) {
    let quantum = Quantum::from_micros(10_000).expect("10 ms is positive");
    assert_eq!(plan.transition_count(quantum), Some(200));
    plan.validate_for_quantum(quantum)
        .expect("canonical plan must validate at 10 ms quantum");
}

/// Normalize the plan into a wire-stable `Program` carrying whichever
/// quantum the scene probe reported. The same plan yields two distinct
/// programs (the rover fixture vs. a 10 ms fixture) but the typed
/// envelope survives.
#[expect(
    clippy::expect_used,
    reason = "fixed fixture literals must remain valid SDK examples"
)]
pub fn program_at(quantum: Quantum, plan: &ScenarioPlan) -> Program {
    let schedule: Vec<ScheduleEntry> = plan
        .steps
        .iter()
        .map(|step| ScheduleEntry::at(step.boundary, step.action.clone()))
        .collect();
    Program::normalize(
        "scenarios/CanonicalFixture",
        quantum,
        plan.duration,
        schedule,
        plan.captures.clone(),
    )
    .expect("canonical plan normalizes for any probed quantum")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SDK-only fixture compiles and runs without the project
    /// compiler, the supervisor, the simulator, or any service.
    /// The harness binary depends on this crate for the
    /// compile-stable pledge of plan §12 Unit 2.
    #[test]
    fn canonical_plan_validates_against_two_non_rover_quanta() {
        let plan = canonical_plan();
        validate_against_five_ms(&plan);
        validate_against_ten_ms(&plan);
    }

    #[test]
    fn canonical_plan_rejects_misaligned_duration() {
        let plan = canonical_plan();
        let quantum = Quantum::from_micros(7_000).expect("7 ms is positive");
        // 7 ms (7_000 µs) does not evenly divide 2 s; the validator
        // must surface DurationNotAligned rather than degrading to
        // an empty transition count.
        let error = plan
            .validate_for_quantum(quantum)
            .expect_err("7 ms quantum must reject a 2 s plan");
        assert!(matches!(
            error,
            phoxal::scenario::PlanValidationError::DurationNotAligned {
                quantum_nanos: 7_000_000,
                ..
            }
        ));
    }

    #[test]
    fn program_normalizes_for_two_distinct_quanta() {
        let plan = canonical_plan();
        let rover = Quantum::from_micros(2_000).expect("rover quantum");
        let lenient = Quantum::from_micros(10_000).expect("10 ms quantum");
        // The same plan normalizes to two distinct programs whose
        // transition counts differ by the chosen quantum. Authors
        // carry neither — the scene probe determines the right one.
        let rover_program = program_at(rover, &plan);
        assert_eq!(rover_program.transition_count(), 1_000);
        let lenient_program = program_at(lenient, &plan);
        assert_eq!(lenient_program.transition_count(), 200);
    }
}
