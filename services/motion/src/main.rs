#[path = "service.rs"]
mod service;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(service::Motion)
}
