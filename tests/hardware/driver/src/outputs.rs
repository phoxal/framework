use crate::api::__contracts::phoxal::fixture::hardware::v1::{
    FixtureObservation, hardware_fixture,
};
use phoxal::runtime::Sample;

/// Fresh observations emitted by the fixture driver.
#[derive(Default)]
#[phoxal::runtime::outputs]
pub struct HardwareFixtureOutputs {
    /// Measured values obtained from the injected fixture device.
    #[phoxal::runtime::outputs::sample(
        port = hardware_fixture::methods::OBSERVATIONS.__sample_port(),
        max_items = 16,
        max_bytes = 4096
    )]
    pub observations: Vec<Sample<FixtureObservation>>,
}
