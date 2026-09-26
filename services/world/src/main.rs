phoxal::api!();

mod config;
mod runtime;
mod validation;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(runtime::World)
}
