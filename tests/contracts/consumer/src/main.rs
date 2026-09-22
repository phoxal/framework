use phoxal::contract::{CallMethod, Empty, MethodDescriptor, MethodShape, ObservationMethod};
use phoxal_contract_owner_fixture::{
    InspectionCommandRequest, InspectionCommandResponse, InspectionEvent, InspectionReadRequest,
    InspectionReadResponse, InspectionSample, InspectionSetpoint, InspectionState,
    InspectionStream, inspection::methods,
};

const _: ObservationMethod<InspectionState> = methods::STATUS;
const _: ObservationMethod<InspectionSample> = methods::SAMPLES;
const _: ObservationMethod<InspectionEvent> = methods::EVENTS;
const _: ObservationMethod<InspectionStream> = methods::RECORDS;
const _: CallMethod<InspectionSetpoint, Empty> = methods::TARGET;
const _: CallMethod<InspectionReadRequest, InspectionReadResponse> = methods::READ;
const _: CallMethod<InspectionCommandRequest, InspectionCommandResponse> = methods::COMMANDS;

fn main() {
    assert_eq!(methods::STATUS.signature().shape, MethodShape::Observation);
    assert!(methods::STATUS.signature().retained_latest);
    assert_eq!(methods::TARGET.signature().shape, MethodShape::Call);
    assert_eq!(
        methods::TARGET
            .signature()
            .lease
            .map(|lease| lease.valid_for_ms()),
        Some(100)
    );
    assert_eq!(methods::READ.signature().method, "Read");
}
