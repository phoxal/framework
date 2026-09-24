use crate::api::kinematics::v1::{JointState, LookupFrameResponse, kinematics};
use phoxal::runtime::Sample;

/// Fresh measured joint products.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct KinematicsOutputs {
    /// One stamped sample for every valid encoder measurement in the cut.
    #[phoxal::runtime::outputs::sample(
        port = kinematics::methods::JOINTS.__sample_port(),
        max_items = 32,
        max_bytes = 16_384
    )]
    pub joints: Vec<Sample<JointState>>,
    /// One correlated result for every admitted frame lookup.
    #[phoxal::runtime::outputs::reply(frame_lookups, max_items = 32, max_bytes = 16_384)]
    pub frame_lookup_replies: Vec<phoxal::runtime::Reply<LookupFrameResponse>>,
}
