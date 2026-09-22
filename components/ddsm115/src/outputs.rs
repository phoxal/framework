use phoxal::robotics::EncoderSample;
use phoxal::runtime::Sample;
use phoxal_component_ddsm115::ddsm115;

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
