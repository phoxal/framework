use crate::api::__contracts::phoxal::robotics::v1::RangeSample;
use crate::api::vl53l1x::v1::vl53l1x;
use phoxal::runtime::Sample;

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
