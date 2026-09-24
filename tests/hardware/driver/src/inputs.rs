use crate::api::__contracts::phoxal::fixture::hardware::v1::{FixtureObservation, FixtureSetpoint};
use phoxal::runtime::input::{Samples, Setpoint};

/// Inputs acquired by the fixture driver at one Runtime boundary.
#[phoxal::runtime::inputs]
pub struct HardwareFixtureInputs {
    /// Samples obtained by the independent fixture acquisition loop.
    #[phoxal::runtime::input(max_items = 16, max_bytes = 4096)]
    pub observations: Samples<FixtureObservation>,
    /// Latest actuator intent received by the fixture transport.
    pub actuator: Setpoint<FixtureSetpoint>,
}
