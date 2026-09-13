use phoxal::runtime::input::Samples;
use phoxal_robotics::EncoderSample;

/// One immutable encoder input cut.
#[phoxal::runtime::inputs]
pub struct KinematicsInputs {
    /// Bounded measurements, retaining each producer's capture stamp.
    #[phoxal::runtime::input(max_items = 32, max_bytes = 262_144)]
    pub encoders: Samples<EncoderSample>,
}
