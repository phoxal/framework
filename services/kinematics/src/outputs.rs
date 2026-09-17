use phoxal::runtime::Sample;
use phoxal_service_kinematics::{JointState, ports};

/// Fresh measured joint products.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct KinematicsOutputs {
    /// One stamped sample for every valid encoder measurement in the cut.
    #[phoxal::runtime::outputs::sample(
        port = ports::JOINTS,
        max_items = 32,
        max_bytes = 16_384
    )]
    pub joints: Vec<Sample<JointState>>,
}
