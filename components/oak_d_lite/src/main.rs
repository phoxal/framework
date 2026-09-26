phoxal::api!();

mod config;
mod runtime;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(runtime::OakDLite)
}
