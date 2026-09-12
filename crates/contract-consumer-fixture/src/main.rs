use prost::Name;

use phoxal_contract_owner_fixture::{
    InspectionCommandRequest, InspectionCommandResponse, InspectionEvent, InspectionReadRequest,
    InspectionReadResponse, InspectionSample, InspectionSetpoint, InspectionState,
    InspectionStream, inspection,
};
use phoxal_port::{
    Commands, Event, PortDescriptor, PortKind, Read, Sample, Setpoint, State, Stream,
};

const _: State<InspectionState> = inspection::STATUS;
const _: Sample<InspectionSample> = inspection::SAMPLES;
const _: Event<InspectionEvent> = inspection::EVENTS;
const _: Stream<InspectionStream> = inspection::RECORDS;
const _: Setpoint<InspectionSetpoint> = inspection::TARGET;
const _: Read<InspectionReadRequest, InspectionReadResponse> = inspection::READ;
const _: Commands<InspectionCommandRequest, InspectionCommandResponse> = inspection::COMMANDS;

fn main() {
    assert_eq!(inspection::STATUS.name(), "status");
    assert_eq!(inspection::SAMPLES.name(), "samples");
    assert_eq!(inspection::EVENTS.name(), "events");
    assert_eq!(inspection::RECORDS.name(), "records");
    assert_eq!(inspection::TARGET.name(), "target");
    assert_eq!(inspection::READ.name(), "read");
    assert_eq!(inspection::COMMANDS.name(), "commands");
    assert_eq!(State::<InspectionState>::KIND, PortKind::State);
    assert_eq!(
        InspectionState::full_name(),
        "example.inspection.v1.InspectionState"
    );
}
