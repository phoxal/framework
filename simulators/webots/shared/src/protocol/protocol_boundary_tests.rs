use super::framing::MAX_FRAME_BYTES;
use super::*;
use std::io::Cursor;

#[test]
fn private_messages_round_trip_through_the_bounded_frame() {
    let request = HostRequest::Event(ControllerEvent::WorldProgress(NativeProgressObservation {
        completed_step: 42,
        elapsed_ns: 504_000_000,
        mode: ObservedNativeMode::RealTime,
    }));
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &request).expect("the private message encodes");
    let decoded =
        read_frame::<_, HostRequest>(&mut Cursor::new(bytes)).expect("the private message decodes");
    assert_eq!(decoded, request);
}

#[test]
fn an_oversized_incoming_frame_is_refused_before_allocation() {
    let bytes = u32::try_from(MAX_FRAME_BYTES + 1)
        .expect("the test bound fits")
        .to_be_bytes();
    assert!(matches!(
        read_frame::<_, HostRequest>(&mut Cursor::new(bytes)),
        Err(LinkError::FrameTooLarge { .. })
    ));
}

#[test]
fn robot_import_budget_admits_real_scene_sizes_before_any_mutation() {
    let source = " ".repeat(MAX_ROBOT_SOURCE_BYTES - 5);
    validate_robot_import("ROBOT", &source).expect("bounded Robot source");
    assert!(validate_robot_import("ROBOT_", &source).is_err());
    let response = HostResponse::Directive(HostDirective::Mutate(NativeMutation::ImportRobot {
        transaction: u64::MAX,
        execution: ExecutionId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0001)
            .expect("execution"),
        definition: "ROBOT".to_owned(),
        source,
    }));
    write_frame(&mut std::io::sink(), &response)
        .expect("preflight leaves room for the wire envelope");
}

#[test]
fn unsupported_native_modes_stay_typed() {
    for observed in [ObservedNativeMode::Run, ObservedNativeMode::Fast] {
        let fault = ControllerFault::UnsupportedMode { observed };
        let bytes = rmp_serde::to_vec_named(&fault).expect("the fault encodes");
        assert_eq!(
            rmp_serde::from_slice::<ControllerFault>(&bytes).expect("the fault decodes"),
            fault
        );
    }
}
