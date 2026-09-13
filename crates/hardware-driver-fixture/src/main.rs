fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(phoxal_hardware_driver_fixture::HardwareFixtureDriver::new())
}
