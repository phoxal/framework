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

use phoxal_port::{PortKind, PortSignature};
use phoxal::scenario::{
    Action, ApplicationAttachment, Capture, FixtureMetadata, FixtureParticipant, Program, Step,
};

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
    let program = Program::normalize(
        "scenarios/RoverForwardTurnStop",
        std::time::Duration::from_secs(3),
        vec![
            Step::new(
                "forward",
                0,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![1],
                },
            ),
            Step::new(
                "turn",
                1,
                Action::Command {
                    service_signature: command_sig(),
                    request_encoded: vec![0x10],
                    label: "turn".to_owned(),
                },
            ),
            Step::new(
                "stop",
                2,
                Action::Setpoint {
                    consumer_signature: setpoint_sig(),
                    encoded_payload: vec![0],
                },
            ),
        ],
        vec![Capture::state("motion", state_sig())],
    )?;

    println!("program digest: {}", program.program_digest);
    println!("transitions:    {}", program.steps.len());
    println!("byte length:    {}", program.byte_length);

    let mut participant = FixtureParticipant::from_program(program)?;
    println!("metadata:       {:#?}", participant.metadata());
    println!("attachments:    {:#?}", vec![ApplicationAttachment::new(
        "rover-1",
        state_sig(),
        "scenarios/RoverForwardTurnStop"
    )]);

    let trace = participant.run();
    println!("trace.passed:   {}", trace.passed());
    println!("step outcomes:  {:#?}", trace.step_outcomes);

    // Mark the command reply after the simulator acknowledges it.
    participant.mark_command_reply("turn");
    println!("sim_expired:    {:?}", participant.expire_simulated_deadlines());

    let _ = FixtureMetadata::from_program; // touched so the import is not flagged
    Ok(())
}