//! Public scenario-plan and normalization behavior through the SDK surface.

use std::error::Error;
use std::io;
use std::time::Duration;

use phoxal::port::{PortKind, PortSignature};
use phoxal::scenario::{
    Action, Capture, Program, Quantum, ScenarioPlan, ScheduleEntry, Step, Validity,
};

fn setpoint_signature() -> PortSignature {
    PortSignature::new(
        "motion",
        "fixture.Motion",
        "Set",
        PortKind::Setpoint,
        "fixture.SetpointRequest",
        "fixture.SetpointResponse",
    )
}

fn state_signature() -> PortSignature {
    PortSignature::new(
        "state",
        "fixture.Motion",
        "State",
        PortKind::State,
        "fixture.StateRequest",
        "fixture.StateResponse",
    )
}

fn quantum(micros: u32) -> Result<Quantum, io::Error> {
    Quantum::from_micros(micros).ok_or_else(|| io::Error::other("quantum must be positive"))
}

fn plan() -> Result<ScenarioPlan, Box<dyn Error>> {
    Ok(ScenarioPlan::with_steps(
        "scene.xml",
        Duration::from_secs(2),
        vec![
            Step::new(
                "drive",
                0,
                Action::setpoint("motion", setpoint_signature(), vec![1], Validity::Permanent)?,
            ),
            Step::new(
                "stop",
                3,
                Action::setpoint("motion", setpoint_signature(), vec![0], Validity::Permanent)?,
            ),
        ],
        vec![Capture::state("motion/state", state_signature())?],
    )?)
}

#[test]
fn one_plan_accepts_multiple_exact_scene_quanta() -> Result<(), Box<dyn Error>> {
    let plan = plan()?;
    for (micros, transitions) in [(5_000, 400), (10_000, 200)] {
        let quantum = quantum(micros)?;
        assert_eq!(plan.transition_count(quantum), Some(transitions));
        plan.validate_for_quantum(quantum)?;
    }
    Ok(())
}

#[test]
fn plan_rejects_a_misaligned_scene_quantum() -> Result<(), Box<dyn Error>> {
    let result = plan()?.validate_for_quantum(quantum(7_000)?);
    assert!(matches!(
        result,
        Err(phoxal::scenario::PlanValidationError::DurationNotAligned {
            quantum_nanos: 7_000_000,
            ..
        })
    ));
    Ok(())
}

#[test]
fn normalization_uses_the_scene_quantum_without_changing_authored_steps()
-> Result<(), Box<dyn Error>> {
    let plan = plan()?;
    for (micros, transitions) in [(2_000, 1_000), (10_000, 200)] {
        let quantum = quantum(micros)?;
        let schedule = plan
            .steps
            .iter()
            .map(|step| ScheduleEntry::at(step.boundary, step.action.clone()))
            .collect();
        let program = Program::normalize(
            "scenarios/Fixture",
            quantum,
            plan.duration,
            schedule,
            plan.captures.clone(),
        )?;
        assert_eq!(program.transition_count(), transitions);
    }
    Ok(())
}
