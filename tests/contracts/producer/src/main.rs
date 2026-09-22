use prost::Name;

use phoxal::contract::MethodDescriptor;
use phoxal_contract_owner_fixture::{InspectionState, inspection::methods};

fn main() {
    assert_eq!(InspectionState::NAME, "InspectionState");
    assert_eq!(
        InspectionState::full_name(),
        "example.inspection.v1.InspectionState"
    );
    assert_eq!(methods::STATUS.signature().method, "Status");
}
