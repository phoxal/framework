mod config;
mod runtime;

phoxal::api!();

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(runtime::HardwareFixtureDriver::new())
}
