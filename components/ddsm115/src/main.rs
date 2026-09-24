phoxal::api!();

mod config;
mod inputs;
mod outputs;
mod runtime;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(runtime::Ddsm115)
}
