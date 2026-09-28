//! The hardware acceptance fixture's endpoint contract. The payloads are
//! fixture-owned identities; the component's capabilities stay physical
//! facts in component.yaml.

use phoxal::contracts::Queue;

#[phoxal::message(package = "phoxal.fixture.hardware.v1")]
pub struct FixtureObservation {
    #[phoxal(tag = 1)]
    pub acquisition_sequence: u64,
    #[phoxal(tag = 2)]
    pub position_rad: f64,
}

#[phoxal::endpoints]
pub struct DriverApi {
    #[phoxal::input(max_items = 16, max_bytes = 4096)]
    acquired: Queue<FixtureObservation>,

    #[phoxal::output(max_items = 16, max_bytes = 4096)]
    observations: Queue<FixtureObservation>,
}
