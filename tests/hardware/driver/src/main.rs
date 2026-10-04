#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

mod config;
phoxal::api!();

mod contract;
mod runtime;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run::<runtime::HardwareFixtureDriver>()
}
