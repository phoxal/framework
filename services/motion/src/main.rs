phoxal::api!();

mod config;
mod drive;
mod inputs;
mod outputs;
mod runtime;
mod validation;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(runtime::Motion)
}
