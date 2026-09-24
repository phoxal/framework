mod config;
mod inputs;
mod outputs;
mod runtime;

phoxal::api!();

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(runtime::HardwareFixtureDriver::new())
}
