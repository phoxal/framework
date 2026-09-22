use phoxal::robotics::EncoderSample;
use phoxal::runtime::input::{Commands, Samples};
use phoxal_service_kinematics::{LookupFrameRequest, LookupFrameResponse, kinematics};

/// One immutable encoder input cut.
#[phoxal::runtime::inputs]
pub struct KinematicsInputs {
    /// Bounded measurements, retaining each producer's capture stamp.
    #[phoxal::runtime::input(max_items = 32, max_bytes = 262_144)]
    pub encoders: Samples<EncoderSample>,
    /// Ordered frame lookups admitted for this invocation.
    #[phoxal::runtime::input(
        port = kinematics::methods::LOOKUP_FRAME.__commands_port(),
        max_items = 32,
        max_bytes = 16_384
    )]
    pub frame_lookups: Commands<LookupFrameRequest, LookupFrameResponse>,
}
