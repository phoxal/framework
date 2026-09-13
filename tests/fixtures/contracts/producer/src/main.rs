use prost::Name;

use phoxal_contract_owner_fixture::{InspectionState, inspection};

fn main() {
    assert_eq!(InspectionState::NAME, "InspectionState");
    assert_eq!(
        InspectionState::full_name(),
        "example.inspection.v1.InspectionState"
    );
    assert_eq!(inspection::STATUS.name(), "status");
}
