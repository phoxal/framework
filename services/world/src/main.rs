mod service;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(service::World)
}
