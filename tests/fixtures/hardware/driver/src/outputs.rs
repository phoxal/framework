use phoxal::runtime::Sample;
use phoxal_hardware_driver_fixture::{FixtureObservation, ports};

/// Fresh observations emitted by the fixture driver.
#[derive(Default)]
#[phoxal::runtime::outputs]
pub struct HardwareFixtureOutputs {
    /// Measured values obtained from the injected fixture device.
    #[phoxal::runtime::outputs::sample(
        port = ports::OBSERVATIONS,
        max_items = 16,
        max_bytes = 4096
    )]
    pub observations: Vec<Sample<FixtureObservation>>,
}
