use phoxal::__generated::prost::Name;

use phoxal::contract::MethodDescriptor;
phoxal::api!();
use api::__contracts::example::inspection::v1::{InspectionState, inspection::methods};

fn main() {
    assert_eq!(InspectionState::NAME, "InspectionState");
    assert_eq!(
        InspectionState::full_name(),
        "example.inspection.v1.InspectionState"
    );
    assert_eq!(methods::STATUS.signature().method, "Status");
}
