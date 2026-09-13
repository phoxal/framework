use phoxal::runtime::Sample;
use phoxal_component_ddsm115::EncoderSample;
use phoxal_component_ddsm115::ports;

#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct Ddsm115Outputs {
    /// Measured position and velocity from the motor's integrated encoder.
    #[phoxal::runtime::outputs::sample(
        port = ports::ENCODER,
        max_items = 16,
        max_bytes = 8_192
    )]
    pub encoder: Vec<Sample<EncoderSample>>,
}
