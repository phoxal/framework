mod config;
mod contract;
mod drive;
mod runtime;
mod validation;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(runtime::Motion)
}
