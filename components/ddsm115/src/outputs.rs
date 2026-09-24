use crate::api::__contracts::phoxal::robotics::v1::EncoderSample;
use crate::api::ddsm115::v1::ddsm115;
use phoxal::runtime::Sample;

#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct Ddsm115Outputs {
    /// Measured position and velocity from the motor's integrated encoder.
    #[phoxal::runtime::outputs::sample(
        port = ddsm115::methods::ENCODER.__sample_port(),
        max_items = 16,
        max_bytes = 8_192
    )]
    pub encoder: Vec<Sample<EncoderSample>>,
}
