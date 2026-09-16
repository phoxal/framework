//! P6 rover example scenario.
//!
//! Reproduces the canonical `forward_turn_stop` rover scenario in
//! terms of the typed P2 program surface. The scenario:
//!  * emits a typed setpoint at quantum 0 with a `phoxal.motion`
//!    setpoint port payload (bytes `1`).
//!  * issues a correlated command at quantum 1 labelled `turn` that
//!    advances the chassis yaw.
//!  * emits a final stop setpoint at quantum 2.
//!  * captures the latest motion state for verification.
//!
//! The expected behaviour matches the experiments the rover
//! integration uses today. Run via:
//!
//! ```sh
//! cargo run --example rover_forward_turn_stop --features scenario
//! ```

use phoxal::scenario::{
    Action, Capture, FixtureParticipant, Program, Quantum, ScheduleEntry, Validity,
};
use phoxal_port::{PortKind, PortSignature};

fn setpoint_sig() -> PortSignature {
    PortSignature::new(
        "motion/cmd",
        "phoxal.motion",
        "Set",
        PortKind::Setpoint,
        "SetpointRequest",
        "SetpointReply",
    )
}

fn command_sig() -> PortSignature {
    PortSignature::new(
        "motion/cmd",
        "phoxal.motion",
        "Turn",
        PortKind::Commands,
        "CommandRequest",
        "CommandReply",
    )
}

fn state_sig() -> PortSignature {
    PortSignature::new(
        "motion/state",
        "phoxal.motion",
        "State",
        PortKind::State,
        "State",
        "State",
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let quantum = Quantum::from_micros(2_000).ok_or("quantum")?;
    let program = Program::normalize(
        "scenarios/RoverForwardTurnStop",
        quantum,
        std::time::Duration::from_secs(3),
        vec![
            ScheduleEntry::at(
                0,
                Action::setpoint("motion", setpoint_sig(), vec![1], Validity::Permanent)?,
            ),
            ScheduleEntry::at(
                1,
                Action::command(
                    "motion",
                    command_sig(),
                    vec![0x10],
                    "turn",
                    std::time::Duration::from_secs(1),
                    std::time::Duration::from_secs(1),
                )?,
            ),
            ScheduleEntry::at(
                2,
                Action::setpoint("motion", setpoint_sig(), vec![0], Validity::Permanent)?,
            ),
        ],
        vec![Capture::state("motion", state_sig())?],
    )?;

    println!("program digest: {}", program.program_digest());
    println!("transitions:    {}", program.transition_count());
    println!("byte length:    {}", program.byte_length());

    // The example exercises plan authoring and program construction
    // only. The synthetic fixture `run` path is removed (Gate B1 of
    // the scenario acceptance review); the controlled phase driver that
    // produces a real trace is the case-host lifecycle that lands in
    // Gate B1/B4. Until then, the example does not invoke the
    // participant beyond identity verification.
    let participant = FixtureParticipant::from_program(program.clone())?;
    let _ = participant.metadata();
    program.verify_identity()?;
    Ok(())
}
