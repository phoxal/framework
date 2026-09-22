use phoxal::robotics::RangeSample;
use phoxal::runtime::Sample;
use phoxal_component_vl53l1x::vl53l1x;

/// VL53L1X observations admitted at one Runtime boundary.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct Vl53l1xOutputs {
    /// Measured time-of-flight range observations.
    #[phoxal::runtime::outputs::sample(
        port = vl53l1x::methods::RANGE.__sample_port(),
        max_items = 16,
        max_bytes = 512
    )]
    pub range: Vec<Sample<RangeSample>>,
}
