mod config;
mod contract;
mod runtime;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run::<runtime::HardwareFixtureDriver>()
}
