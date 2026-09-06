use super::*;
use phoxal::bus::BusError;

#[test]
fn removing_during_a_transition_keeps_the_orderly_shutdown_handshake() {
    assert!(matches!(
        authority_exit(
            SimulatorError::AttachmentInactive,
            Some(SimulationAttachmentPhase::Removing),
            "completed transition",
        ),
        ControllerLoopExit::Removing
    ));
    for phase in [None, Some(SimulationAttachmentPhase::Active)] {
        assert!(matches!(
            authority_exit(SimulatorError::AttachmentInactive, phase, "boundary"),
            ControllerLoopExit::SupervisorLost { .. }
        ));
    }
}

#[test]
fn binary_abi_has_exactly_two_required_flags() {
    let parsed = Args::try_parse_from([
        "robot-controller",
        "--connect",
        "tcp/127.0.0.1:7447",
        "--host-connect",
        "tcp://127.0.0.1:1234",
    ])
    .expect("fixed ABI parses");
    assert_eq!(parsed.host_connect, "tcp://127.0.0.1:1234");
}

#[test]
fn drive_policy_distinguishes_new_reused_expired_source_loss_and_missing() {
    assert_eq!(
        classify_selection(true, true, false, 1, true),
        ActuationSelection::SelectedNew
    );
    assert_eq!(
        classify_selection(true, false, true, 1, false),
        ActuationSelection::Reused
    );
    assert_eq!(
        classify_selection(false, false, true, 1, false),
        ActuationSelection::None {
            reason: NoActuationReason::Expired
        }
    );
    assert_eq!(
        classify_selection(false, false, false, 0, false),
        ActuationSelection::None {
            reason: NoActuationReason::SourceAbsent
        }
    );
    assert_eq!(
        classify_selection(false, false, false, 1, false),
        ActuationSelection::None {
            reason: NoActuationReason::Missing
        }
    );
}

#[test]
fn late_attachment_begins_from_its_immutable_world_boundary() {
    let attached = WorldProgress::at(42, 12_000_000).expect("late world boundary");
    assert_eq!(activation_progress(None, 7, attached), Some(attached));
    assert_eq!(activation_progress(Some(7), 7, attached), None);
}

#[test]
fn geared_motor_commands_use_the_same_native_domain_as_rendered_limits() {
    assert_eq!(
        dispatch_motor(
            MotorCommand::Position,
            &api::component::motor::Command::Position(6.0),
            3.0,
        )
        .expect("position command"),
        MotorAction::Position(2.0)
    );
    assert_eq!(
        dispatch_motor(
            MotorCommand::Velocity,
            &api::component::motor::Command::Velocity(6.0),
            3.0,
        )
        .expect("velocity command"),
        MotorAction::Velocity(2.0)
    );
    assert_eq!(
        dispatch_motor(
            MotorCommand::Torque,
            &api::component::motor::Command::Torque(2.0),
            3.0,
        )
        .expect("torque command"),
        MotorAction::Torque(6.0)
    );
}

#[test]
fn parking_attempts_every_motor_after_one_stop_fails() {
    struct Probe {
        name: &'static str,
        parked: bool,
    }
    let mut motors = [
        Probe {
            name: "first",
            parked: false,
        },
        Probe {
            name: "second",
            parked: false,
        },
    ];
    let result = stop_every(
        &mut motors,
        |motor| motor.name.to_owned(),
        |motor| {
            motor.parked = true;
            if motor.name == "first" {
                anyhow::bail!("injected motor failure");
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(motors.iter().all(|motor| motor.parked));
}

#[test]
fn completed_transition_publishes_outputs_before_step() {
    let order = std::cell::RefCell::new(Vec::new());
    publish_completed_transition(
        || {
            order.borrow_mut().push("output");
            Ok(())
        },
        || {
            order.borrow_mut().push("step");
            Ok(())
        },
    )
    .expect("both publications succeed");
    assert_eq!(*order.borrow(), ["output", "step"]);
}

#[test]
fn lossless_publication_refusal_is_a_controller_local_protocol_fault() {
    let output_fault = publish_completed_transition(
        || {
            Err(anyhow::Error::new(SimulatorError::Bus(
                BusError::WouldBlock {
                    topic: api::topics().drive().state().owner().key().to_owned(),
                },
            )))
        },
        || panic!("StepEvent must not be attempted after an output refusal"),
    )
    .expect_err("a refused output faults the controller");
    assert!(matches!(
        output_fault,
        ControllerFault::Protocol { ref detail }
            if detail.contains("typed output publication failed")
                && detail.contains("would block")
    ));

    let step_fault = publish_completed_transition(
        || Ok(()),
        || {
            Err(SimulatorError::Bus(BusError::WouldBlock {
                topic: phoxal::simulation::api::topics()
                    .step()
                    .owner()
                    .key()
                    .to_owned(),
            }))
        },
    )
    .expect_err("a refused StepEvent faults the controller");
    assert!(matches!(
        step_fault,
        ControllerFault::Protocol { ref detail }
            if detail.contains("StepEvent publication failed")
                && detail.contains("would block")
    ));
}
